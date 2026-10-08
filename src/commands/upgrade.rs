use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectEntry};
use crate::state::workflow::VANILLA_AGENT;

use super::hooks_install;
use super::serve_install::{self, Refresh};
use super::skills;
use super::upgrade_restart;

/// Upgrade a single project: reinstall hooks, bootstrap state, migrate any
/// pre-global-tier bundled copies away, and project the project's own
/// customs for each harness in use. Feature worktrees are untouched except by
/// that migration, which removes only pm-owned files.
/// The global asset tier is installed separately (see [`upgrade`]) since it
/// is shared by every project.
pub fn upgrade_project(project_root: &Path) -> Result<Vec<String>> {
    let mut updated: Vec<String> = Vec::new();

    // Install hooks at the user level, moving any out of the project files
    let _ = hooks_install::install(Some(project_root))?;
    updated.push("hooks".to_string());

    // Bootstrap information store and state repo (both idempotent)
    super::docs::bootstrap(project_root)?;
    super::state_cmd::init(project_root)?;
    // Migrate docs submodule to regular files if needed
    if super::docs::migrate_docs_submodule(project_root).unwrap_or(false) {
        updated.push("docs (migrated from submodule)".to_string());
    } else {
        updated.push("docs".to_string());
    }

    // One-shot: drop the bundled copies earlier releases installed per
    // project, so they can't shadow the global tier.
    if !skills::is_migrated(project_root) {
        let removed = skills::migrate_project_to_global(project_root, false)?;
        if !removed.is_empty() {
            // Only the `.pm/workflows/` ones are recoverable: the rest sit in
            // generated, gitignored dirs of the project repo.
            updated.push(format!(
                "{} bundled copies removed (any under .pm/workflows/ are in .pm/ git \
                 history; commit the deletion with `pm state push`)",
                removed.len()
            ));
        }
    }

    let renamed = super::vanilla_rename::migrate(project_root, false)?;

    // Harnesses read from their own dirs, not the canonical store. The
    // projection's own lines (which name any hand-written harness file a
    // canonical one replaced) follow the summary.
    let notes = skills::project_assets(project_root, false)?;
    updated.push("projections".to_string());

    let summary = format!("Upgraded {} for main", updated.join(", "));
    let mut lines = vec![summary];
    if !renamed.is_empty() {
        lines.push(format!(
            "Vanilla agents {} now launch as '{VANILLA_AGENT}'",
            agent_list(&renamed)
        ));
    }
    lines.extend(notes);
    Ok(lines)
}

/// Dry-run variant of [`upgrade_project`]: report what would change without
/// writing anything. Returns one `Would …` line per action that would be
/// taken, empty when the project is fully up to date, and whether the hooks
/// agents launch with would change.
fn upgrade_project_dry_run(project_root: &Path) -> Result<(Vec<String>, bool)> {
    let mut actions = hooks_install::install_dry_run(Some(project_root))?;
    let hooks_change = !actions.is_empty();

    // Information store (.pm/docs/) bootstrap
    for path in super::docs::bootstrap_dry_run(project_root) {
        actions.push(format!(
            "Would create {}",
            display_path(project_root, &path)
        ));
    }

    // State repo (.pm/) init
    if super::state_cmd::would_init(project_root) {
        let pm_dir = paths::pm_dir(project_root);
        actions.push(format!(
            "Would initialise state repo in {}",
            display_path(project_root, &pm_dir)
        ));
    }

    // Submodule migration
    if super::docs::would_migrate_docs_submodule(project_root) {
        actions.push("Would migrate .pm/docs/ from submodule to regular files".to_string());
    }

    // Bundled copies left by earlier releases
    if !skills::is_migrated(project_root) {
        actions.extend(removal_lines(skills::migrate_project_to_global(
            project_root,
            true,
        )?));
    }

    let renamed = super::vanilla_rename::migrate(project_root, true)?;
    if !renamed.is_empty() {
        actions.push(format!(
            "Would relaunch vanilla agents {} as '{VANILLA_AGENT}'",
            agent_list(&renamed)
        ));
    }

    // Projections (compares the canonical store as it is on disk now)
    actions.extend(skills::project_assets(project_root, true)?);

    Ok((actions, hooks_change))
}

/// `agents` as `'default' (login), …`.
fn agent_list(agents: &[(String, String)]) -> String {
    let names: Vec<String> = agents
        .iter()
        .map(|(scope, name)| format!("'{name}' ({scope})"))
        .collect();
    names.join(", ")
}

/// One line with a file count per worktree a migration would remove bundled
/// copies from, and one per path elsewhere (`.pm/workflows/`). `removed` is
/// relative to the project root.
fn removal_lines(removed: Vec<PathBuf>) -> Vec<String> {
    let mut per_worktree: Vec<(String, usize)> = Vec::new();
    let mut elsewhere = Vec::new();
    for path in removed {
        let top = path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        if top == paths::PM_DIR_NAME {
            elsewhere.push(format!("Would remove {}", path.display()));
        } else if let Some(entry) = per_worktree.iter_mut().find(|(w, _)| *w == top) {
            entry.1 += 1;
        } else {
            per_worktree.push((top, 1));
        }
    }
    let mut lines: Vec<String> = per_worktree
        .into_iter()
        .map(|(worktree, n)| {
            format!(
                "Would remove {n} bundled file{} under {worktree}/",
                if n == 1 { "" } else { "s" }
            )
        })
        .collect();
    lines.extend(elsewhere);
    lines
}

/// Format a path relative to `project_root` when possible, otherwise display
/// the full path. Keeps dry-run output concise and stable across machines.
fn display_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// Install the global tier, then upgrade every project registered in
/// `projects_dir`: a summary line per project, a broken one included.
fn upgrade_projects(projects_dir: &Path) -> Result<Vec<String>> {
    let projects = ProjectEntry::list(projects_dir)?;

    if projects.is_empty() {
        let mut lines = install_global_lines(false);
        lines.push("No registered projects".to_string());
        return Ok(lines);
    }

    let mut lines = install_global_lines(false);
    for (name, entry) in &projects {
        // Skip legacy entries with non-portable roots — calling
        // `to_portable` on a relative `PathBuf` would `debug_assert!` in
        // debug builds and silently misbehave in release. `pm self-update`
        // chains through here, so a single bad entry must not break the
        // command users run to *pick up* the fix.
        if !crate::path_utils::is_portable(&entry.root) {
            lines.push(format!(
                "{name}: skipped (non-portable root \"{}\" — run `pm state backfill` for details)",
                entry.root
            ));
            continue;
        }

        // Migrate absolute paths to portable ~/… format on re-save
        let portable = crate::path_utils::to_portable(&entry.root_path());
        if portable != entry.root {
            let migrated = ProjectEntry {
                root: portable,
                ..entry.clone()
            };
            migrated.save(projects_dir, name)?;
        }

        let root = entry.root_path();
        if !root.exists() {
            lines.push(format!("{name}: skipped (root does not exist)"));
            continue;
        }
        match upgrade_project(&root) {
            Ok(project_lines) => {
                let mut project_lines = project_lines.into_iter();
                if let Some(summary) = project_lines.next() {
                    lines.push(format!("{name}: {summary}"));
                }
                lines.extend(project_lines.map(|l| format!("  {l}")));
            }
            Err(e) => lines.push(format!("{name}: error: {e}")),
        }
    }
    Ok(lines)
}

/// Dry-run variant of [`upgrade_projects`], and whether the hooks agents
/// launch with would change in any project.
fn upgrade_projects_dry_run(projects_dir: &Path) -> Result<(Vec<String>, bool)> {
    let projects = ProjectEntry::list(projects_dir)?;
    let mut hooks_change = false;

    if projects.is_empty() {
        let mut lines = install_global_lines(true);
        lines.push("No registered projects".to_string());
        return Ok((lines, hooks_change));
    }

    let mut lines = install_global_lines(true);
    for (name, entry) in &projects {
        // Same guard as in upgrade_projects: never resolve a non-portable root
        // against the caller's CWD.
        if !crate::path_utils::is_portable(&entry.root) {
            lines.push(format!(
                "{name}: skipped (non-portable root \"{}\" — run `pm state backfill` for details)",
                entry.root
            ));
            continue;
        }

        let root = entry.root_path();
        if !root.exists() {
            lines.push(format!("{name}: skipped (root does not exist)"));
            continue;
        }
        let actions = upgrade_project_dry_run(&root).map(|(actions, hooks)| {
            hooks_change |= hooks;
            actions
        });
        match actions {
            Ok(actions) if actions.is_empty() => {
                lines.push(format!("{name}: up to date"));
            }
            Ok(actions) => {
                lines.push(format!("{name}:"));
                for action in actions {
                    lines.push(format!("  {action}"));
                }
            }
            Err(e) => lines.push(format!("{name}: error: {e}")),
        }
    }
    Ok((lines, hooks_change))
}

/// Install (or preview installing) the shared global asset tier, and
/// refresh the `pm serve` LaunchAgent if one is installed. A failure here —
/// no resolvable home, say — is reported as a line rather than aborting the
/// per-project work that follows.
fn install_global_lines(dry_run: bool) -> Vec<String> {
    let result = if dry_run {
        skills::install_global_dry_run()
    } else {
        skills::install_global()
    };
    let mut lines = match result {
        Ok(lines) => lines,
        Err(e) => vec![format!("global assets: error: {e}")],
    };
    let refreshed = paths::home_dir().and_then(|home| {
        serve_install::refresh(
            &home,
            &paths::global_config_dir()?,
            dry_run,
            serve_install::reload,
        )
    });
    match refreshed {
        Ok(Refresh::NotInstalled | Refresh::Current) => {}
        Ok(Refresh::Updated) if dry_run => {
            lines.push("Would update pm serve LaunchAgent".to_string())
        }
        Ok(Refresh::Updated) => lines.push("updated pm serve LaunchAgent".to_string()),
        Err(e) => lines.push(format!("pm serve LaunchAgent: error: {e}")),
    }
    lines
}

/// Upgrade every registered project, then restart the agents left stale
/// ([`upgrade_restart`]). The global tier, hooks and `pm serve` LaunchAgent
/// are the machine's, so upgrading one project alone would leave the others
/// half-upgraded. When `dry_run` is `true`, preview changes without writing
/// anything.
pub fn upgrade(dry_run: bool, tmux_server: Option<&str>) -> Result<Vec<String>> {
    upgrade_in(&paths::global_projects_dir()?, dry_run, tmux_server)
}

/// [`upgrade`] of the projects registered in `projects_dir`.
fn upgrade_in(
    projects_dir: &Path,
    dry_run: bool,
    tmux_server: Option<&str>,
) -> Result<Vec<String>> {
    let (mut lines, hooks_change) = if dry_run {
        upgrade_projects_dry_run(projects_dir)?
    } else {
        (upgrade_projects(projects_dir)?, false)
    };
    // What agents launch with that the upgrade would change: the global
    // tier's definitions and baseline, and the hooks.
    let launch_changes =
        dry_run && (hooks_change || skills::install_global_dry_run().is_ok_and(|l| !l.is_empty()));
    let global = GlobalConfig::load_or_default();
    let (scopes, unread) = upgrade_restart::scopes(projects_dir);
    lines.extend(unread);
    lines.extend(upgrade_restart::restart_stale(
        &scopes,
        &global,
        dry_run,
        tmux_server,
    ));
    if launch_changes && global.upgrade.restarts_agents() {
        lines.push(upgrade_restart::DRY_RUN_CAVEAT.to_string());
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup_project(dir: &std::path::Path) -> PathBuf {
        let root = dir.to_path_buf();
        fs::create_dir_all(root.join(".pm").join("features")).unwrap();
        fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        root
    }

    fn write_feature_toml(root: &std::path::Path, name: &str) {
        let content = format!(
            r#"status = "wip"
branch = "{name}"
worktree = "{name}"
base = ""
pr = ""
context = ""
created = "2026-01-01T00:00:00Z"
last_active = "2026-01-01T00:00:00Z"
"#
        );
        fs::write(
            root.join(".pm")
                .join("features")
                .join(format!("{name}.toml")),
            content,
        )
        .unwrap();
    }

    #[test]
    fn upgrade_relaunches_registered_default_agents_as_plain() {
        use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        skills::install_global().unwrap();
        let agents_dir = paths::agents_dir(&root);
        let mut registry = AgentRegistry::default();
        registry.register(
            "default",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: "sid".to_string(),
                window_name: "default".to_string(),
                active: true,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, "login").unwrap();

        let dry = upgrade_project_dry_run(&root).unwrap().0;
        assert!(
            dry.contains(&"Would relaunch vanilla agents 'default' (login) as 'plain'".to_string()),
            "{dry:?}"
        );
        assert_eq!(AgentRegistry::load(&agents_dir, "login").unwrap(), registry);

        let lines = upgrade_project(&root).unwrap();
        assert!(
            lines.contains(&"Vanilla agents 'default' (login) now launch as 'plain'".to_string()),
            "{lines:?}"
        );
        let after = AgentRegistry::load(&agents_dir, "login").unwrap();
        assert_eq!(
            after
                .get("default")
                .unwrap()
                .effective_definition("default"),
            VANILLA_AGENT
        );
    }

    #[test]
    fn upgrade_installs_hooks_and_state_but_no_bundled_copies() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        // The tier `pm upgrade` installs before touching any project — the
        // per-project pass deliberately doesn't.
        skills::install_global().unwrap();

        let summary = upgrade_project(&root).unwrap().join("\n");
        assert!(summary.contains("hooks"), "{summary}");
        assert!(summary.contains("docs"), "{summary}");
        assert!(summary.contains("for main"), "{summary}");
        assert!(hooks_install::is_installed_for(Harness::ClaudeCode).unwrap());
        assert!(skills::is_migrated(&root));

        // Bundled assets go to the global tier, never into the project.
        let main = paths::main_worktree(&root);
        assert!(!main.join(".agents").exists());
        assert!(!main.join(".claude/agents").exists());
        assert!(!paths::workflows_dir(&root).exists());
        let home = paths::home_dir().unwrap();
        assert!(home.join(".agents/agents/reviewer.md").exists());
        assert!(home.join(".claude/agents/reviewer.md").exists());
        assert!(home.join(".agents/pm-baseline.md").exists());
        assert!(
            paths::global_workflows_dir()
                .unwrap()
                .join("implement-and-review/workflow.md")
                .exists()
        );
    }

    #[test]
    fn upgrade_migrates_pre_global_layout_and_keeps_live_state() {
        // A project last touched by a release that installed bundled copies
        // per project: everything under `main/.claude/` and `.pm/workflows/`,
        // old-generation hook commands, a user custom def, live
        // registry/feature/message state. One upgrade must leave it working.
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        let main = paths::main_worktree(&root);
        let claude = main.join(".claude");
        fs::create_dir_all(claude.join("agents")).unwrap();
        fs::create_dir_all(claude.join("skills/pm")).unwrap();
        fs::write(claude.join("agents/reviewer.md"), "stale bundled def").unwrap();
        fs::write(claude.join("agents/custom.md"), "user's own def").unwrap();
        fs::write(claude.join("skills/pm/SKILL.md"), "stale skill").unwrap();
        fs::write(claude.join("pm-baseline.md"), "stale baseline").unwrap();
        fs::write(
            claude.join("settings.json"),
            r#"{"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]},{"hooks":[{"type":"command","command":"pm claude hooks stop","timeout":86400}]}],"SessionStart":[{"hooks":[{"type":"command","command":"pm claude hooks session-start"}]}]}}"#,
        )
        .unwrap();
        let solo = paths::workflows_dir(&root).join("solo");
        fs::create_dir_all(&solo).unwrap();
        fs::write(
            solo.join("config.toml"),
            "description = \"old\"\nagents = [\"claude\"]\nbrief_agents = [\"claude\"]\n",
        )
        .unwrap();
        fs::write(solo.join("workflow.md"), "# solo\n## claude\n").unwrap();
        let my_flow = paths::workflows_dir(&root).join("my-flow");
        fs::create_dir_all(&my_flow).unwrap();
        fs::write(my_flow.join("config.toml"), "description = \"mine\"\n").unwrap();
        let agents_toml = root.join(".pm/agents/main.toml");
        fs::create_dir_all(agents_toml.parent().unwrap()).unwrap();
        let registry =
            "[agents.orchestrator]\nagent_type = \"agent\"\nsession_id = \"abc\"\nactive = true\n";
        fs::write(&agents_toml, registry).unwrap();
        write_feature_toml(&root, "login");
        let feat = root.join("login");
        fs::create_dir_all(feat.join(".claude/agents")).unwrap();
        fs::write(feat.join(".claude/agents/reviewer.md"), "seeded stale").unwrap();
        let feature_toml = fs::read(root.join(".pm/features/login.toml")).unwrap();
        let msg = root.join(".pm/messages/login/implementer/from-user/001.md");
        fs::create_dir_all(msg.parent().unwrap()).unwrap();
        fs::write(&msg, "hello").unwrap();

        let dry = upgrade_project_dry_run(&root).unwrap().0;
        assert!(
            dry.contains(&"Would remove 3 bundled files under main/".to_string()),
            "{dry:?}"
        );
        assert!(
            dry.contains(&"Would remove 1 bundled file under login/".to_string()),
            "{dry:?}"
        );
        assert!(
            dry.contains(&"Would remove .pm/workflows/solo".to_string()),
            "{dry:?}"
        );
        assert!(
            dry.contains(&"Would remove pm hooks from main/.claude/settings.json".to_string()),
            "{dry:?}"
        );
        assert!(claude.join("agents/reviewer.md").exists(), "dry-run wrote");

        let summary = upgrade_project(&root).unwrap().join("\n");
        assert!(summary.contains("bundled copies removed"), "{summary}");

        // Every bundled copy is gone from main and the feature …
        assert!(!claude.join("agents/reviewer.md").exists());
        assert!(!claude.join("skills/pm").exists());
        assert!(!claude.join("pm-baseline.md").exists());
        assert!(!feat.join(".claude/agents/reviewer.md").exists());
        assert!(!solo.exists());
        // … while customs and live state are untouched.
        assert_eq!(
            fs::read_to_string(claude.join("agents/custom.md")).unwrap(),
            "user's own def"
        );
        assert!(my_flow.join("config.toml").exists());
        assert_eq!(fs::read_to_string(&agents_toml).unwrap(), registry);
        assert_eq!(
            fs::read(root.join(".pm/features/login.toml")).unwrap(),
            feature_toml
        );
        assert_eq!(fs::read_to_string(&msg).unwrap(), "hello");

        // Hooks moved to the user level; the project's own settings and
        // hooks stayed behind.
        assert!(hooks_install::is_installed_for(Harness::ClaudeCode).unwrap());
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(claude.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["permissions"]["allow"][0], "Read");
        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1, "{settings}");
        assert_eq!(stop[0]["hooks"][0]["command"], "echo mine");
        assert!(
            settings["hooks"].get("SessionStart").is_none(),
            "{settings}"
        );

        // `solo` now resolves from the global tier, and a second run is a no-op.
        assert!(crate::state::workflow::exists(&root, "solo"));
        let actions = upgrade_project_dry_run(&root).unwrap().0;
        assert!(actions.is_empty(), "second dry-run not empty: {actions:?}");
    }

    #[test]
    fn migrated_project_keeps_its_customs_on_later_upgrades() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        upgrade_project(&root).unwrap();

        // A bundled-named file written after the marker is the user's.
        let custom = paths::main_worktree(&root).join(".agents/agents/reviewer.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "my reviewer").unwrap();
        let user_flow = paths::workflows_dir(&root).join("solo");
        fs::create_dir_all(&user_flow).unwrap();
        fs::write(user_flow.join("config.toml"), "description = \"my solo\"\n").unwrap();

        upgrade_project(&root).unwrap();
        assert_eq!(fs::read_to_string(&custom).unwrap(), "my reviewer");
        assert_eq!(
            fs::read_to_string(user_flow.join("config.toml")).unwrap(),
            "description = \"my solo\"\n"
        );
        // And it is projected where the harness reads it.
        assert_eq!(
            fs::read_to_string(paths::main_worktree(&root).join(".claude/agents/reviewer.md"))
                .unwrap(),
            "my reviewer"
        );
    }

    /// Every file under `dir`, relative to it, with its bytes.
    fn snapshot(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    out.push((path.strip_prefix(base).unwrap().to_path_buf(), Vec::new()));
                    walk(&path, base, out);
                } else {
                    out.push((
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn upgrade_leaves_feature_worktrees_untouched() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        // Main has customs and settings the feature lacks or holds stale.
        let main = paths::main_worktree(&root);
        for rel in [
            ".agents/agents/custom.md",
            ".agents/skills/howto/SKILL.md",
            ".claude/settings.json",
        ] {
            let path = main.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "main's").unwrap();
        }
        write_feature_toml(&root, "my-feat");
        let feat = root.join("my-feat");
        for rel in [
            ".agents/agents/custom.md",
            ".claude/settings.json",
            "src/lib.rs",
        ] {
            let path = feat.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "feature's").unwrap();
        }
        let before = snapshot(&feat);

        upgrade_project(&root).unwrap();

        assert_eq!(snapshot(&feat), before);
        // Main's own projection still ran.
        assert!(main.join(".claude/agents/custom.md").exists());
        assert!(upgrade_project_dry_run(&root).unwrap().0.is_empty());
    }

    #[test]
    fn upgrade_bootstraps_docs() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        upgrade_project(&root).unwrap();

        let docs_dir = root.join(".pm").join("docs");
        assert!(docs_dir.join("categories.toml").exists());
        assert!(docs_dir.join("todo.md").exists());
        // Docs are tracked by the parent .pm/ state repo, not a separate git repo
        assert!(!docs_dir.join(".git").exists());
        assert!(root.join(".pm").join(".git").exists());
    }

    // --- Dry-run tests ---

    #[test]
    fn dry_run_reports_actions_on_fresh_project_and_writes_nothing() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let actions = upgrade_project_dry_run(&root).unwrap().0;
        let joined = actions.join("\n");
        assert!(
            joined.contains("Would create"),
            "missing docs create lines, got: {joined}"
        );
        assert!(
            joined.contains("Would initialise state repo"),
            "missing state repo line, got: {joined}"
        );

        let main = paths::main_worktree(&root);
        assert!(!main.join(".claude").exists());
        assert!(!main.join(".agents").exists());
        assert!(!root.join(".pm").join("docs").exists());
        assert!(!root.join(".pm").join(".git").exists());
        assert!(!skills::is_migrated(&root));
    }

    #[test]
    fn dry_run_returns_empty_when_up_to_date_and_reports_drifted_projections() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());
        upgrade_project(&root).unwrap();
        assert!(upgrade_project_dry_run(&root).unwrap().0.is_empty());

        let main = paths::main_worktree(&root);
        let custom = main.join(".agents/agents/custom.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "custom def").unwrap();

        let actions = upgrade_project_dry_run(&root).unwrap().0;
        assert!(
            actions.iter().any(|a| a.starts_with("Would project")),
            "expected projection line, got: {actions:?}"
        );
        assert!(!main.join(".claude/agents/custom.md").exists());
    }

    #[test]
    fn dry_run_lists_each_project_then_the_stale_agents() {
        let server = crate::testing::TestServer::new();
        let dir = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let (root, project_name) = server.setup_project_with_feature(dir.path(), "login");
        ProjectEntry {
            root: root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        }
        .save(registry.path(), &project_name)
        .unwrap();
        let session = crate::tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&root, &session, "login", "reviewer");
        skills::install_global().unwrap();
        upgrade_project(&root).unwrap();
        let dry_run = || upgrade_in(registry.path(), true, server.name()).unwrap();
        assert_eq!(dry_run(), [format!("{project_name}: up to date")]);

        crate::state::runtime::write_launch_stamp(&root, "login", "reviewer", "old").unwrap();
        assert_eq!(
            dry_run(),
            [
                format!("{project_name}: up to date"),
                format!("{session}: Would restart agent 'reviewer'"),
            ],
        );

        let custom = paths::main_worktree(&root).join(".agents/agents/custom.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "custom def").unwrap();
        assert!(
            !dry_run().contains(&upgrade_restart::DRY_RUN_CAVEAT.to_string()),
            "a projection changes nothing agents launch with"
        );
        let settings = paths::main_worktree(&root).join(".claude/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(
            &settings,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"pm claude hooks stop"}]}]}}"#,
        )
        .unwrap();
        let lines = dry_run();
        assert_eq!(
            lines.last().map(String::as_str),
            Some(upgrade_restart::DRY_RUN_CAVEAT),
            "{lines:#?}"
        );
    }

    // --- upgrade_projects tests ---

    /// Write a registry entry directly to disk, bypassing the
    /// ProjectEntry::save validation. Used to simulate legacy bad entries
    /// from before the relative-root guard existed.
    fn write_raw_registry_entry(projects_dir: &std::path::Path, name: &str, root: &str) {
        fs::create_dir_all(projects_dir).unwrap();
        fs::write(
            projects_dir.join(format!("{name}.toml")),
            format!("root = \"{root}\"\nmain_branch = \"main\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn upgrade_installs_the_global_tier_once_and_upgrades_every_project() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("registry");
        for name in ["one", "two"] {
            let root = setup_project(&dir.path().join(name));
            ProjectEntry {
                root: root.to_string_lossy().to_string(),
                main_branch: "main".to_string(),
                repo_url: None,
                state_remote: None,
            }
            .save(&projects_dir, name)
            .unwrap();
        }

        let lines = upgrade_projects(&projects_dir).unwrap();
        let joined = lines.join("\n");
        assert!(
            joined.contains("one: Upgraded") && joined.contains("two: Upgraded"),
            "{joined}"
        );
        // The global install is reported once, not per project.
        assert!(
            lines.iter().filter(|l| l.contains("(global)")).count() <= 1
                || lines
                    .iter()
                    .position(|l| l.contains("one:"))
                    .is_some_and(|i| lines[..i].iter().all(|l| !l.contains(": Upgraded"))),
            "{joined}"
        );
        for name in ["one", "two"] {
            assert!(skills::is_migrated(&dir.path().join(name)));
        }
        assert!(
            paths::home_dir()
                .unwrap()
                .join(".agents/agents/main.md")
                .exists()
        );
    }

    #[test]
    fn upgrade_skips_legacy_non_portable_root() {
        // Regression: a single bad entry must not break `pm upgrade` or
        // (by extension) `pm self-update`, which is the command users would
        // run to *pick up* this fix.
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("registry");
        write_raw_registry_entry(&projects_dir, "exo-bench", "exo-bench");

        let good_root = setup_project(&dir.path().join("good"));
        ProjectEntry {
            root: good_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        }
        .save(&projects_dir, "good")
        .unwrap();

        let lines = upgrade_projects(&projects_dir).unwrap();
        let joined = lines.join("\n");
        assert!(
            joined.contains("exo-bench: skipped (non-portable root")
                && joined.contains("pm state backfill"),
            "expected non-portable skip line, got: {joined}"
        );
        assert!(
            joined.contains("good:") && !joined.contains("good: skipped"),
            "good project should not be skipped, got: {joined}"
        );

        let (lines, _) = upgrade_projects_dry_run(&projects_dir).unwrap();
        assert!(
            lines
                .join("\n")
                .contains("exo-bench: skipped (non-portable root"),
            "expected non-portable skip line in dry-run, got: {lines:?}"
        );
    }
}
