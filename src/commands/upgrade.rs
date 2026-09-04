use std::path::Path;

use crate::error::Result;
use crate::state::paths;
use crate::state::project::ProjectEntry;

use super::hooks_install;
use super::seed;
use super::skills;

/// Upgrade a single project: reinstall hooks, bootstrap state, migrate any
/// pre-global-tier bundled copies away, project the project's own customs
/// for each harness in use, then re-seed every active feature worktree.
/// The global asset tier is installed separately (see [`upgrade_all`] and
/// [`upgrade`]) since it is shared by every project.
pub fn upgrade_project(project_root: &Path) -> Result<String> {
    let mut updated = Vec::new();

    // Install hooks
    let _ = hooks_install::install(project_root)?;
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
        updated.push(format!(
            "{} bundled copies removed (previous content is in .pm/ git history; \
             commit with `pm state push`)",
            removed.len()
        ));
    }

    // Harnesses read from their own dirs, not the canonical store.
    let _ = skills::project_assets(project_root, false)?;
    updated.push("projections".to_string());

    // Re-seed each active feature worktree
    let features_dir = paths::features_dir(project_root);
    let features = crate::state::feature::FeatureState::list(&features_dir)?;
    let mut feature_count = 0;
    for (name, _state) in &features {
        let feature_worktree = project_root.join(name);
        if feature_worktree.is_dir() {
            seed::seed_feature_assets(project_root, &feature_worktree)?;
            feature_count += 1;
        }
    }

    let parts = updated.join(", ");
    if feature_count > 0 {
        Ok(format!(
            "Upgraded {parts} for main + {feature_count} feature{}",
            if feature_count == 1 { "" } else { "s" }
        ))
    } else {
        Ok(format!("Upgraded {parts} for main"))
    }
}

/// Dry-run variant of [`upgrade_project`]: report what would change without
/// writing anything. Returns one `Would …` line per action that would be
/// taken; an empty `Vec` means the project is fully up to date. The public
/// [`upgrade`] dispatcher is responsible for translating an empty result
/// into the user-facing `Up to date` line.
pub fn upgrade_project_dry_run(project_root: &Path) -> Result<Vec<String>> {
    let mut actions = Vec::new();

    // Hooks
    if let Some(line) = hooks_install::install_dry_run(project_root)? {
        actions.push(line);
    }

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
        for path in skills::migrate_project_to_global(project_root, true)? {
            actions.push(format!("Would remove {}", path.display()));
        }
    }

    // Projections (compares the canonical store as it is on disk now)
    actions.extend(skills::project_assets(project_root, true)?);

    // Feature worktrees: only report each feature whose seeded assets differ
    let features_dir = paths::features_dir(project_root);
    let features = crate::state::feature::FeatureState::list(&features_dir)?;
    for (name, _state) in &features {
        let feature_worktree = project_root.join(name);
        if !feature_worktree.is_dir() {
            continue;
        }
        if seed::seed_feature_assets_would_change(project_root, &feature_worktree)? {
            actions.push(format!("Would re-seed harness assets in feature '{name}'"));
        }
    }

    Ok(actions)
}

/// Format a path relative to `project_root` when possible, otherwise display
/// the full path. Keeps dry-run output concise and stable across machines.
fn display_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// Upgrade all registered projects.
/// Returns one summary line per project.
pub fn upgrade_all() -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    upgrade_all_with_dir(&projects_dir)
}

/// Testable inner function that takes an explicit projects directory.
pub fn upgrade_all_with_dir(projects_dir: &Path) -> Result<Vec<String>> {
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
            Ok(summary) => lines.push(format!("{name}: {summary}")),
            Err(e) => lines.push(format!("{name}: error: {e}")),
        }
    }
    Ok(lines)
}

/// Dry-run variant of [`upgrade_all`]: preview what would change for every
/// registered project without writing anything.
pub fn upgrade_all_dry_run() -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    upgrade_all_dry_run_with_dir(&projects_dir)
}

/// Testable inner function that takes an explicit projects directory.
pub fn upgrade_all_dry_run_with_dir(projects_dir: &Path) -> Result<Vec<String>> {
    let projects = ProjectEntry::list(projects_dir)?;

    if projects.is_empty() {
        let mut lines = install_global_lines(true);
        lines.push("No registered projects".to_string());
        return Ok(lines);
    }

    let mut lines = install_global_lines(true);
    for (name, entry) in &projects {
        // Same guard as in upgrade_all: never resolve a non-portable root
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
        match upgrade_project_dry_run(&root) {
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
    Ok(lines)
}

/// Install (or preview installing) the shared global asset tier. A failure
/// here — no resolvable home, say — is reported as a line rather than
/// aborting the per-project work that follows.
fn install_global_lines(dry_run: bool) -> Vec<String> {
    let result = if dry_run {
        skills::install_global_dry_run()
    } else {
        skills::install_global()
    };
    match result {
        Ok(lines) => lines,
        Err(e) => vec![format!("global assets: error: {e}")],
    }
}

/// Upgrade either the current project (default) or all projects (--all).
/// When `dry_run` is `true`, preview changes without writing anything.
pub fn upgrade(all: bool, dry_run: bool) -> Result<Vec<String>> {
    if all {
        if dry_run {
            upgrade_all_dry_run()
        } else {
            upgrade_all()
        }
    } else {
        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
        let mut lines = install_global_lines(dry_run);
        if dry_run {
            lines.extend(upgrade_project_dry_run(&project_root)?);
            if lines.is_empty() {
                lines.push("Up to date".to_string());
            }
        } else {
            lines.push(upgrade_project(&project_root)?);
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn upgrade_installs_hooks_and_state_but_no_bundled_copies() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("hooks"), "{summary}");
        assert!(summary.contains("docs"), "{summary}");
        assert!(summary.contains("for main"), "{summary}");
        assert!(hooks_install::is_installed(&root).unwrap());
        assert!(skills::is_migrated(&root));

        // Bundled assets live only in the global tier now.
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
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"pm claude hooks stop","timeout":86400}]}],"SessionStart":[{"hooks":[{"type":"command","command":"pm claude hooks session-start"}]}]}}"#,
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

        let dry = upgrade_project_dry_run(&root).unwrap();
        assert!(
            dry.contains(&"Would remove main/.claude/agents/reviewer.md".to_string()),
            "{dry:?}"
        );
        assert!(
            dry.contains(&"Would remove login/.claude/agents/reviewer.md".to_string()),
            "{dry:?}"
        );
        assert!(
            dry.contains(&"Would remove .pm/workflows/solo".to_string()),
            "{dry:?}"
        );
        assert!(claude.join("agents/reviewer.md").exists(), "dry-run wrote");

        let summary = upgrade_project(&root).unwrap();
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

        // Hooks were rewritten to the current command spelling.
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(claude.join("settings.json")).unwrap())
                .unwrap();
        for (event, cmd) in [
            ("Stop", "pm harness hooks stop"),
            ("SessionStart", "pm harness hooks session-start"),
        ] {
            let entries = settings["hooks"][event].as_array().unwrap();
            assert_eq!(entries.len(), 1, "{event}");
            assert_eq!(entries[0]["hooks"][0]["command"].as_str().unwrap(), cmd);
        }

        // `solo` now resolves from the global tier, and a second run is a no-op.
        assert!(crate::state::workflow::exists(&root, "solo"));
        let actions = upgrade_project_dry_run(&root).unwrap();
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

    #[test]
    fn upgrade_reseeds_feature_worktrees() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        write_feature_toml(&root, "my-feat");
        fs::create_dir_all(root.join("my-feat")).unwrap();
        // A project custom is what a feature now gets seeded with.
        let custom = paths::main_worktree(&root).join(".agents/agents/custom.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "custom def").unwrap();

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("1 feature"), "{summary}");

        let feat = root.join("my-feat");
        assert!(feat.join(".claude/settings.json").exists());
        assert_eq!(
            fs::read_to_string(feat.join(".agents/agents/custom.md")).unwrap(),
            "custom def"
        );
        assert_eq!(
            fs::read_to_string(feat.join(".claude/agents/custom.md")).unwrap(),
            "custom def"
        );
        assert!(!feat.join(".claude/agents/reviewer.md").exists());
    }

    #[test]
    fn upgrade_reports_multiple_features_and_skips_missing_worktrees() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        for name in &["feat-a", "feat-b", "feat-c"] {
            write_feature_toml(&root, name);
            fs::create_dir_all(root.join(name)).unwrap();
        }
        write_feature_toml(&root, "orphan");

        let summary = upgrade_project(&root).unwrap();
        assert!(summary.contains("3 features"), "{summary}");
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

        let actions = upgrade_project_dry_run(&root).unwrap();
        let joined = actions.join("\n");
        assert!(
            joined.contains("Would install pm hooks"),
            "missing hooks line, got: {joined}"
        );
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
        assert!(upgrade_project_dry_run(&root).unwrap().is_empty());

        let main = paths::main_worktree(&root);
        let custom = main.join(".agents/agents/custom.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "custom def").unwrap();

        let actions = upgrade_project_dry_run(&root).unwrap();
        assert!(
            actions.iter().any(|a| a.starts_with("Would project")),
            "expected projection line, got: {actions:?}"
        );
        assert!(!main.join(".claude/agents/custom.md").exists());
    }

    #[test]
    fn dry_run_reports_feature_reseed_when_stale() {
        let dir = tempdir().unwrap();
        let root = setup_project(dir.path());

        upgrade_project(&root).unwrap();

        write_feature_toml(&root, "stale-feat");
        let feat_claude = root.join("stale-feat").join(".claude");
        fs::create_dir_all(&feat_claude).unwrap();
        fs::write(feat_claude.join("settings.json"), "{}").unwrap();

        let actions = upgrade_project_dry_run(&root).unwrap();
        assert!(
            actions
                .iter()
                .any(|a| a == "Would re-seed harness assets in feature 'stale-feat'"),
            "expected feature line, got: {actions:?}"
        );
    }

    // --- upgrade_all_with_dir tests ---

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
    fn upgrade_all_installs_the_global_tier_once_and_upgrades_every_project() {
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

        let lines = upgrade_all_with_dir(&projects_dir).unwrap();
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
    fn upgrade_all_skips_legacy_non_portable_root() {
        // Regression: a single bad entry must not break `upgrade --all` or
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

        let lines = upgrade_all_with_dir(&projects_dir).unwrap();
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

        let lines = upgrade_all_dry_run_with_dir(&projects_dir).unwrap();
        assert!(
            lines
                .join("\n")
                .contains("exo-bench: skipped (non-portable root"),
            "expected non-portable skip line in dry-run, got: {lines:?}"
        );
    }
}
