//! Each harness in use's hooks and trust: pm's entries installed and
//! well-formed, its hooks trusted, and the worktrees its agents launch in.

use std::path::Path;

use super::config::{agents_configs, worktree_harnesses};
use super::{Fix, FixAction, Issue, IssueKind};
use crate::commands::harness_check::{self, Problem, ProblemKind};
use crate::commands::skills;
use crate::error::Result;
use crate::harness::Probe;
use crate::state::paths;
use crate::state::project::harness_config;

/// Main-scope findings about each harness in use's hooks: pm's entries
/// missing from its user-level file, entries in a shape it would ignore,
/// pm hooks it has not been told to trust, and worktrees whose agents run
/// on it that it would stop at a trust prompt for. The trust findings exist
/// because codex fails silently on both counts. Only harnesses in use, although the install
/// writes every supported harness's file: a trust finding for a harness
/// none of this project's agents run on would be noise.
pub(super) fn hook_issues(project_root: &Path, probe: Probe) -> Result<Vec<Issue>> {
    hook_issues_in(project_root, &paths::home_dir()?, probe)
}

/// [`hook_issues`] against an explicit `home`.
fn hook_issues_in(project_root: &Path, home: &Path, probe: Probe) -> Result<Vec<Issue>> {
    let worktree_harnesses = worktree_harnesses(project_root)?;
    let (project, global) = agents_configs(project_root)?;
    let config = harness_config(Some(project_root));
    let mut issues = Vec::new();
    for harness in skills::harnesses_in_use(project_root)? {
        for Problem { kind, message } in
            harness_check::harness_problems(harness, &config, home, probe)?
        {
            // The default harness is in use whether or not an agent is on it.
            if kind == ProblemKind::Unusable
                && !harness_check::has_agents(harness, &project, &global)
            {
                continue;
            }
            issues.push(match kind {
                ProblemKind::Unusable => Issue {
                    kind: IssueKind::HarnessUnusable,
                    message: format!("agents configured for {harness} cannot run: {message}"),
                    fix: Fix::None,
                },
                ProblemKind::LoopNotInstalled | ProblemKind::StatusHooksMissing => Issue {
                    kind: IssueKind::HooksNotInstalled,
                    message,
                    fix: Fix::Auto(FixAction::InstallStopHook),
                },
                ProblemKind::HooksMalformed => Issue {
                    kind: IssueKind::HooksMalformed,
                    message,
                    fix: Fix::None,
                },
                ProblemKind::HookUntrusted
                | ProblemKind::ResetHookUntrusted
                | ProblemKind::StatusHookUntrusted => Issue {
                    kind: IssueKind::HookUntrusted,
                    message,
                    fix: Fix::None,
                },
            });
        }
        for (wt, needed) in &worktree_harnesses {
            if needed.contains(&harness) && !harness.worktree_trusted(home, wt) {
                issues.push(Issue {
                    kind: IssueKind::WorktreeUntrusted,
                    message: format!(
                        "{} is not trusted by {harness}; it will stop at a trust prompt on launch",
                        wt.strip_prefix(project_root).unwrap_or(wt).display()
                    ),
                    fix: Fix::Auto(FixAction::TrustWorktree {
                        harness,
                        path: wt.clone(),
                    }),
                });
            }
        }
    }
    Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::test_support::*;
    use crate::commands::doctor::{Depth, Finding, diagnose, doctor};
    use crate::commands::hooks_install;
    use crate::harness::Harness;
    use crate::state::agent::{AgentRegistry, AgentType};
    use crate::state::feature::FeatureState;
    use crate::state::project::ProjectConfig;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn no_features_still_reports_main_scope_findings() {
        // Hook and trust findings are about the machine, not a feature, so
        // a project that has none yet must still hear about them.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();
        // main runs that agent, so its worktree needs codex's trust.
        let mut registry = AgentRegistry::default();
        registry.register(
            "reviewer",
            crate::state::agent::AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: "reviewer".to_string(),
                active: false,
                agent_definition: None,
                harness: Harness::ClaudeCode,
                spawned_at: None,
            },
        );
        registry
            .save(&paths::agents_dir(&project_path), "main")
            .unwrap();

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let main_kinds: Vec<IssueKind> = findings
            .iter()
            .filter(|f| f.feature() == "main")
            .flat_map(|f| f.issues())
            .map(|i| i.kind())
            .collect();
        assert!(
            main_kinds.iter().any(|k| matches!(
                k,
                IssueKind::HooksNotInstalled
                    | IssueKind::HookUntrusted
                    | IssueKind::WorktreeUntrusted
            )),
            "{main_kinds:?}"
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines[0].starts_with("Checked main and 0 feature(s): ")
                && lines[0].contains("issue(s) found"),
            "{lines:?}"
        );
    }

    #[test]
    fn opencode_in_use_is_checked_for_its_plugin_and_installed_release() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let home = dir.path().join("home");

        // Not in use: nothing about opencode, whatever is installed.
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert!(
            issues.iter().all(|i| !i.message().contains("opencode")),
            "{:?}",
            issues.iter().map(Issue::message).collect::<Vec<_>>()
        );

        use_opencode(&project_path, "reviewer", "opencode v2.0.17");
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::HarnessUnusable),
            vec![
                "agents configured for opencode cannot run: installed opencode is `opencode \
                 v2.0.17`; pm's never-idle plugin needs 2.0.18 or later"
            ]
        );
        let plugin = messages(&issues, IssueKind::HooksNotInstalled)
            .into_iter()
            .filter(|m| m.contains("opencode"))
            .collect::<Vec<_>>();
        assert_eq!(plugin.len(), 1, "{plugin:?}");
        assert!(
            plugin[0].starts_with("pm plugin for opencode missing or out of date in "),
            "{plugin:?}"
        );
        assert!(
            plugin[0].contains(".config/opencode/plugins/pm-never-idle"),
            "{plugin:?}"
        );
        // No trust gate on opencode.
        assert!(
            issues
                .iter()
                .all(|i| i.kind() != IssueKind::WorktreeUntrusted
                    && i.kind() != IssueKind::HookUntrusted)
        );

        use_opencode(&project_path, "reviewer", "opencode v2.0.18");
        for (path, content) in Harness::OpenCode.plugin_files(&home) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert!(
            issues.iter().all(|i| !i.message().contains("opencode")),
            "{:?}",
            issues.iter().map(Issue::message).collect::<Vec<_>>()
        );

        // A copy an upgrade has not replaced yet.
        let (index, _) = Harness::OpenCode.plugin_files(&home).remove(0);
        std::fs::write(index, "export default {}").unwrap();
        let issues = hook_issues_in(&project_path, &home, Probe::Fresh).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::HooksNotInstalled)
                .iter()
                .filter(|m| m.contains("opencode"))
                .count(),
            1
        );
    }

    #[test]
    fn codex_in_use_is_checked_for_hooks_trust_and_worktree_trust() {
        let _guard = crate::testing::CODEX_CONFIG_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);

        // Trust is per worktree: main runs a registered agent whose
        // definition (not its key) is the reviewer, login's workflow team
        // includes one, api has only an implementer.
        let stopped = |name: &str, definition: Option<&str>| crate::state::agent::AgentEntry {
            agent_type: AgentType::Agent,
            session_id: String::new(),
            window_name: name.to_string(),
            active: false,
            agent_definition: definition.map(str::to_string),
            harness: Harness::ClaudeCode,
            spawned_at: None,
        };
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::default();
        registry.register("qa", stopped("qa", Some("reviewer")));
        registry.save(&agents_dir, "main").unwrap();
        let features_dir = paths::features_dir(&project_path);
        let mut login = FeatureState::load(&features_dir, "login").unwrap();
        login.workflow = Some("implement-and-review".to_string());
        login.save(&features_dir, "login").unwrap();
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project_path,
                &TestServer::registry_dir(&project_path),
                "api",
                server.name(),
            ),
        )
        .unwrap();
        let mut registry = AgentRegistry::default();
        registry.register("implementer", stopped("implementer", None));
        registry.save(&agents_dir, "api").unwrap();

        fn hook_kinds<'a>(issues: impl Iterator<Item = &'a Issue>) -> Vec<(IssueKind, String)> {
            issues
                .filter(|i| {
                    matches!(
                        i.kind(),
                        IssueKind::HooksNotInstalled
                            | IssueKind::HooksMalformed
                            | IssueKind::HookUntrusted
                            | IssueKind::WorktreeUntrusted
                    )
                })
                .map(|i| (i.kind(), i.message().to_string()))
                .collect()
        }
        let kinds = |findings: &[Finding]| -> Vec<(IssueKind, String)> {
            hook_kinds(
                findings
                    .iter()
                    .filter(|f| f.feature() == "main")
                    .flat_map(|f| f.issues().iter()),
            )
        };

        // Claude Code only: nothing codex-related, although the install
        // wrote codex's file too.
        assert!(hooks_install::is_installed_for(Harness::Codex).unwrap());
        assert!(
            kinds(&diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap())
                .is_empty()
        );

        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("reviewer".to_string(), "codex".to_string());
        config.save(&pm_dir).unwrap();

        // Against a home nothing has installed into (the shared test home
        // is written by every concurrent `init`): hooks missing, worktrees
        // untrusted.
        let bare_home = dir.path().join("bare-home");
        let found = hook_kinds(
            hook_issues_in(&project_path, &bare_home, Probe::Fresh)
                .unwrap()
                .iter(),
        );
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![
                IssueKind::HooksNotInstalled,
                IssueKind::HooksNotInstalled,
                IssueKind::WorktreeUntrusted,
                IssueKind::WorktreeUntrusted
            ],
            "{found:?}"
        );
        assert!(found[0].1.contains(".claude/settings.json"), "{found:?}");
        assert!(found[1].1.contains(".codex/hooks.json"), "{found:?}");
        assert!(
            found[2].1.starts_with("main is not trusted by codex"),
            "{found:?}"
        );
        assert!(
            found[3].1.starts_with("login is not trusted by codex"),
            "{found:?}"
        );

        // --fix installs the hooks and trusts both worktrees; hook trust is
        // codex's alone to grant, so it remains.
        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("not trusted by codex")),
            "{lines:?}"
        );
        assert!(hooks_install::is_installed_for(Harness::Codex).unwrap());
        let found =
            kinds(&diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap());
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![IssueKind::HookUntrusted; 4],
            "{found:?}"
        );
        assert!(found[0].1.contains("pm's Stop hook"), "{found:?}");
        assert!(found[1].1.contains("pm's SessionStart hook"), "{found:?}");
        assert!(
            found[2].1.contains("pm's UserPromptSubmit hook"),
            "{found:?}"
        );
        assert!(
            found[3].1.contains(
                "pm's status hooks (PermissionRequest, PreToolUse, PostToolUse, Interrupt), \
                 so an agent waiting on you reads as busy"
            ),
            "{found:?}"
        );

        // Trust recorded the way codex writes it, at pm's entries' positions,
        // in a home of the test's own: trust written to the shared test home
        // would outlive this test and reach every other one.
        let trusted_home = dir.path().join("trusted-home");
        hooks_install::install_in(&trusted_home, None, false).unwrap();
        for wt in ["main", "login"] {
            Harness::Codex
                .trust_worktree(&trusted_home, &project_path.join(wt))
                .unwrap();
        }
        let codex_hooks = trusted_home.join(".codex/hooks.json");
        let root = hooks_install::user_hooks_root(Harness::Codex, &trusted_home)
            .unwrap()
            .unwrap();
        let mut trust = String::new();
        for (event, markers) in hooks_install::pm_events(Harness::Codex) {
            let (i, j) = hooks_install::pm_hook_position(&root, event, markers).unwrap();
            let snake = match event {
                "Stop" => "stop",
                "SessionStart" => "session_start",
                "UserPromptSubmit" => "user_prompt_submit",
                "PermissionRequest" => "permission_request",
                "PreToolUse" => "pre_tool_use",
                "PostToolUse" => "post_tool_use",
                "Interrupt" => "interrupt",
                other => panic!("no codex trust key for {other}"),
            };
            trust.push_str(&format!(
                "[hooks.state.\"{}:{snake}:{i}:{j}\"]\ntrusted_hash = \"sha256:t\"\n",
                codex_hooks.display()
            ));
        }
        let config_toml = trusted_home.join(".codex/config.toml");
        let existing = std::fs::read_to_string(&config_toml).unwrap();
        std::fs::write(&config_toml, format!("{existing}\n{trust}")).unwrap();
        let found = hook_kinds(
            hook_issues_in(&project_path, &trusted_home, Probe::Fresh)
                .unwrap()
                .iter(),
        );
        assert!(found.is_empty(), "{found:?}");

        // A flat hooks.json registers nothing in codex: flagged, not "installed".
        let flat = bare_home.join(".codex/hooks.json");
        std::fs::create_dir_all(flat.parent().unwrap()).unwrap();
        std::fs::write(
            &flat,
            r#"{"hooks":{"Stop":[{"type":"command","command":"pm harness hooks stop"}]}}"#,
        )
        .unwrap();
        let found = hook_kinds(
            hook_issues_in(&project_path, &bare_home, Probe::Fresh)
                .unwrap()
                .iter(),
        );
        assert_eq!(
            found.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![
                IssueKind::HooksNotInstalled,
                IssueKind::HooksMalformed,
                IssueKind::HooksNotInstalled,
                IssueKind::WorktreeUntrusted,
                IssueKind::WorktreeUntrusted
            ],
            "{found:?}"
        );
        assert!(found[2].1.contains(".codex/hooks.json"), "{found:?}");
    }

    #[test]
    fn fix_strips_stale_project_hooks() {
        // A project last upgraded by a release that wrote the hooks into
        // main/.claude/settings.json (and seeded them into the feature).
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let legacy = r#"{"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"pm claude hooks stop"}]}]}}"#;
        for wt in ["main", "login"] {
            let claude = project_path.join(wt).join(".claude");
            std::fs::create_dir_all(&claude).unwrap();
            std::fs::write(claude.join("settings.json"), legacy).unwrap();
        }

        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        let main = findings.iter().find(|f| f.feature() == "main").unwrap();
        let stale: Vec<&str> = main
            .issues()
            .iter()
            .filter(|i| i.kind() == IssueKind::StaleProjectHooks)
            .map(|i| i.message())
            .collect();
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(stale[0].contains("main/.claude/settings.json"), "{stale:?}");
        assert!(
            stale[1].contains("login/.claude/settings.json"),
            "{stale:?}"
        );
        assert!(
            !main
                .issues()
                .iter()
                .any(|i| i.kind() == IssueKind::HooksNotInstalled)
        );

        let lines = doctor(&project_path, &projects_dir, true, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("fixed") && l.contains("pm hooks still in")),
            "got: {lines:?}"
        );
        for wt in ["main", "login"] {
            let settings: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(project_path.join(wt).join(".claude/settings.json"))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(settings["permissions"]["allow"][0], "Read", "{wt}");
            assert!(settings.get("hooks").is_none(), "{wt}: {settings}");
        }
        let findings = diagnose(&project_path, &projects_dir, server.name(), Depth::Quick).unwrap();
        assert!(
            findings.iter().all(|f| f.feature() != "main"),
            "main still has issues after fix"
        );
    }
}
