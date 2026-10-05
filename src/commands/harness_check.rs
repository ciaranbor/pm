//! Whether the harness an agent is configured for can run it. One detection,
//! two consumers: `pm doctor` reports the problems as findings, and feature
//! creation refuses on them before anything exists.
//!
//! Everything here is read offline from config and the harness's own files,
//! plus one `--version` call per harness.

use std::path::Path;

use crate::commands::{agent_spawn, hooks_install};
use crate::error::{PmError, Result};
use crate::harness::{Harness, Probe};
use crate::state::paths;
use crate::state::project::{
    AgentSettings, AgentsConfig, GlobalConfig, HarnessConfig, ProjectConfig, WILDCARD_AGENT,
    resolve_agent_settings, resolve_harness_config,
};
use crate::state::workflow::is_vanilla;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    /// The binary can't be run or is too old. The message is the reason
    /// alone, for the caller to frame.
    Unusable,
    /// pm's hooks or plugin are missing or out of date.
    LoopNotInstalled,
    /// A hooks entry in a shape the harness registers nothing for.
    HooksMalformed,
    /// The harness has no trust entry for a pm hook of the never-idle loop.
    HookUntrusted,
    /// The harness has no trust entry for pm's blocked-reset hook. The agent
    /// still runs, so this alone never refuses a team.
    ResetHookUntrusted,
    /// The harness has no trust entry for a pm status hook: the agent runs,
    /// but a dialog waiting on the user reads as busy.
    StatusHookUntrusted,
    /// A pm status hook is missing, with the same effect.
    StatusHooksMissing,
}

/// One reason agents on a harness would not start, or would start and never
/// wake for a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: ProblemKind,
    pub message: String,
}

/// What stands between `harness` as installed under `home` and a working
/// agent, whichever agent it is.
pub fn harness_problems(
    harness: Harness,
    config: &HarnessConfig,
    home: &Path,
    probe: Probe,
) -> Result<Vec<Problem>> {
    let mut problems = Vec::new();
    let mut push = |kind, message| problems.push(Problem { kind, message });

    if let Some(reason) = harness.unusable_reason(config, probe) {
        push(ProblemKind::Unusable, reason);
    }
    let shown = hooks_install::install_location(harness, home)
        .map(|path| crate::path_utils::to_portable(&path))
        .unwrap_or_default();
    if !hooks_install::stale_plugin_files(harness, home).is_empty() {
        push(
            ProblemKind::LoopNotInstalled,
            format!(
                "pm plugin for {harness} missing or out of date in {shown} (run `pm harness \
                 hooks install`)"
            ),
        );
    }
    let root = hooks_install::user_hooks_root(harness, home)?;
    if let Some(root) = &root {
        for event in harness.malformed_hook_events(root) {
            push(
                ProblemKind::HooksMalformed,
                format!(
                    "{shown} `hooks.{event}` holds a bare hook object; {harness} registers \
                     nothing for it — wrap it as {{\"hooks\": [...]}}"
                ),
            );
        }
        let untrusted = |(event, markers): &(&str, &[&str])| {
            hooks_install::pm_hook_position(root, event, markers).is_some_and(|(entry, hook)| {
                !harness.hook_trusted(config, home, event, entry, hook)
            })
        };
        let (status, others): (Vec<_>, Vec<_>) = hooks_install::pm_events(harness)
            .into_iter()
            .filter(untrusted)
            .partition(|(_, markers)| {
                markers.contains(&hooks_install::PM_WAITING_MARKER)
                    || markers.contains(&hooks_install::PM_DIALOG_MARKER)
            });
        for (event, _) in others {
            push(
                if event == hooks_install::USER_PROMPT_EVENT {
                    ProblemKind::ResetHookUntrusted
                } else {
                    ProblemKind::HookUntrusted
                },
                format!(
                    "{harness} has not trusted pm's {event} hook, so it silently does not \
                     run: {}",
                    harness.hook_trust_remedy()
                ),
            );
        }
        if !status.is_empty() {
            let events: Vec<&str> = status.iter().map(|(event, _)| *event).collect();
            push(
                ProblemKind::StatusHookUntrusted,
                format!(
                    "{harness} has not trusted pm's status hooks ({}), so an agent waiting on \
                     you reads as busy: {}",
                    events.join(", "),
                    harness.hook_trust_remedy()
                ),
            );
        }
    }
    if !hooks_install::hooks_registered(harness, home, root.as_ref()) {
        push(
            ProblemKind::LoopNotInstalled,
            format!("pm hooks not installed in {shown} (run `pm harness hooks install`)"),
        );
    } else if harness.user_settings_file(home).is_some() {
        let missing = hooks_install::missing_status_hooks(harness, root.as_ref());
        if !missing.is_empty() {
            push(
                ProblemKind::StatusHooksMissing,
                format!(
                    "pm status hooks ({}) not installed in {shown}, so an agent waiting on you \
                     reads as busy (run `pm harness hooks install`)",
                    missing.join(", ")
                ),
            );
        }
        let missing = hooks_install::missing_dialog_hooks(harness, root.as_ref());
        if !missing.is_empty() {
            push(
                ProblemKind::StatusHooksMissing,
                format!(
                    "pm dialog hook ({}) not installed in {shown}, so a dialog can be answered \
                     at the terminal only (run `pm harness hooks install`)",
                    missing.join(", ")
                ),
            );
        }
    }
    Ok(problems)
}

/// Whether `settings` lacks the `[agents.models]` row its harness refuses to
/// spawn without; the value is the parenthetical about a row the resolution
/// dropped, empty when there was none.
pub fn missing_model_row(settings: &AgentSettings) -> Option<String> {
    if !settings.harness.requires_model() || settings.model.is_some() {
        return None;
    }
    let dropped: Vec<String> = settings.dropped_model.clone().into_iter().collect();
    Some(agent_spawn::notes_suffix(&dropped))
}

/// What `settings` holds that its harness refuses to spawn with.
pub fn row_issues(settings: &AgentSettings) -> Vec<String> {
    settings.harness.row_issues(
        settings.model.as_deref(),
        settings.permission_mode.as_deref(),
    )
}

/// Whether any agent launches on `harness`: a configured row names it, or
/// an agent without a row lands on it.
pub fn has_agents(harness: Harness, project: &AgentsConfig, global: &AgentsConfig) -> bool {
    project
        .harness
        .keys()
        .chain(global.harness.keys())
        .map(String::as_str)
        .chain([WILDCARD_AGENT])
        .any(|key| agent_spawn::configured_harness(key, project, global).ok() == Some(harness))
}

/// Refuse a workflow whose team can't all run: one line per problem, each
/// naming the member and its harness, for every failing member at once.
pub fn check_team(project_root: &Path, workflow: &str, team: &[String]) -> Result<()> {
    let config = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let global = GlobalConfig::load_or_default();
    let problems = team_problems(
        &config.agents,
        &global.agents,
        &resolve_harness_config(&config.harness, &global.harness),
        &paths::main_worktree(project_root),
        &paths::home_dir()?,
        team,
    )?;
    if problems.lines.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = problems
        .lines
        .iter()
        .map(|line| format!("  {line}"))
        .collect();
    Err(PmError::SafetyCheck(format!(
        "workflow '{workflow}' cannot start: {} of {} team member(s) cannot run on their \
         harness. Nothing was created.\n{}\nFix these (`pm doctor` lists them too), or change \
         the member's `[agents.harness]` row.",
        problems.failing,
        team.len(),
        lines.join("\n")
    )))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TeamProblems {
    /// How many members have at least one line.
    failing: usize,
    /// One per problem, in team order, each naming its member.
    lines: Vec<String>,
}

fn team_problems(
    project: &AgentsConfig,
    global: &AgentsConfig,
    config: &HarnessConfig,
    main: &Path,
    home: &Path,
    team: &[String],
) -> Result<TeamProblems> {
    let mut by_harness: Vec<(Harness, Vec<Problem>)> = Vec::new();
    let mut out = TeamProblems::default();
    for member in team {
        let before = out.lines.len();
        match resolve_agent_settings(project, global, member) {
            Err(e) => out.lines.push(format!("{member}: {e}")),
            Ok(settings) => {
                let harness = settings.harness;
                let cached = match by_harness.iter().position(|(h, _)| *h == harness) {
                    Some(i) => i,
                    None => {
                        by_harness.push((
                            harness,
                            harness_problems(harness, config, home, Probe::Fresh)?,
                        ));
                        by_harness.len() - 1
                    }
                };
                for problem in by_harness[cached].1.iter().filter(|p| {
                    !matches!(
                        p.kind,
                        ProblemKind::ResetHookUntrusted
                            | ProblemKind::StatusHookUntrusted
                            | ProblemKind::StatusHooksMissing
                    )
                }) {
                    out.lines
                        .push(format!("{member} ({harness}): {}", problem.message));
                }
                if let Some(dropped) = missing_model_row(&settings) {
                    out.lines.push(format!(
                        "{member} ({harness}): no [agents.models] row, which {harness} agents \
                         need{dropped}; set `[agents.models] {member} = \
                         \"<provider>/<model>\"`"
                    ));
                }
                for issue in row_issues(&settings) {
                    out.lines.push(format!("{member} ({harness}): {issue}"));
                }
                if !is_vanilla(member) && !harness.definition_projected(main, home, member) {
                    out.lines.push(format!(
                        "{member} ({harness}): definition '{member}' is not projected for \
                         {harness}, so the agent would start without its role (run `pm upgrade`)"
                    ));
                }
            }
        }
        if out.lines.len() > before {
            out.failing += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn team(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// A home with pm's hooks and plugin installed for every harness.
    fn home_with_hooks(dir: &Path) -> std::path::PathBuf {
        let home = dir.join("home");
        hooks_install::install_in(&home, None, false).unwrap();
        home
    }

    /// A main worktree with every test member's definition projected for
    /// every harness that projects them.
    fn projected_main(dir: &Path) -> std::path::PathBuf {
        let main = dir.join("main");
        for harness in Harness::SUPPORTED {
            let agents = main.join(harness.config_dir()).join("agents");
            std::fs::create_dir_all(&agents).unwrap();
            for member in ["implementer", "reviewer", "qa"] {
                std::fs::write(agents.join(format!("{member}.md")), "# stub").unwrap();
            }
        }
        main
    }

    fn mixed_agents() -> AgentsConfig {
        let mut agents = AgentsConfig::default();
        agents.harness.insert("reviewer".into(), "codex".into());
        agents.harness.insert("qa".into(), "opencode".into());
        agents
    }

    fn opencode_reporting(dir: &Path, version: &str) -> HarnessConfig {
        let mut config = HarnessConfig::default();
        config.opencode.binary = Some(crate::testing::fake_opencode(dir, version, 0));
        config
    }

    #[test]
    fn every_failing_member_is_named_with_its_harness_and_what_is_missing() {
        let dir = tempdir().unwrap();
        let home = home_with_hooks(dir.path());
        let config = opencode_reporting(dir.path(), "opencode v2.0.17");

        let problems = team_problems(
            &mixed_agents(),
            &AgentsConfig::default(),
            &config,
            &projected_main(dir.path()),
            &home,
            &team(&["implementer", "reviewer", "qa"]),
        )
        .unwrap();
        let remedy = Harness::Codex.hook_trust_remedy();
        assert_eq!(
            problems.lines,
            [
                format!(
                    "reviewer (codex): codex has not trusted pm's Stop hook, so it silently \
                     does not run: {remedy}"
                ),
                format!(
                    "reviewer (codex): codex has not trusted pm's SessionStart hook, so it \
                     silently does not run: {remedy}"
                ),
                "qa (opencode): installed opencode is `opencode v2.0.17`; pm's never-idle \
                 plugin needs 2.0.18 or later"
                    .to_string(),
                "qa (opencode): no [agents.models] row, which opencode agents need; set \
                 `[agents.models] qa = \"<provider>/<model>\"`"
                    .to_string(),
            ]
        );
        assert_eq!(problems.failing, 2);
    }

    #[test]
    fn a_team_whose_harnesses_are_all_ready_passes() {
        let dir = tempdir().unwrap();
        let home = home_with_hooks(dir.path());
        let mut config = opencode_reporting(dir.path(), "opencode v2.0.18");
        config.codex.bypass_hook_trust = Some(true);
        let mut agents = mixed_agents();
        agents.models.insert("qa".into(), "local/qwen".into());

        let problems = team_problems(
            &agents,
            &AgentsConfig::default(),
            &config,
            &projected_main(dir.path()),
            &home,
            &team(&["implementer", "reviewer", "qa"]),
        )
        .unwrap();
        assert_eq!(problems, TeamProblems::default());
    }

    #[test]
    fn missing_status_hooks_are_reported_but_never_refuse_a_team() {
        let dir = tempdir().unwrap();
        let home = home_with_hooks(dir.path());
        let settings = home.join(".claude/settings.json");
        let mut root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let hooks = root["hooks"].as_object_mut().unwrap();
        hooks.remove("Notification");
        hooks.remove("StopFailure");
        // The dialog hook's entry alone, the status hook's beside it kept.
        hooks["PermissionRequest"]
            .as_array_mut()
            .unwrap()
            .retain(|e| {
                !e.to_string()
                    .contains(crate::commands::hooks_install::PM_DIALOG_MARKER)
            });
        std::fs::write(&settings, root.to_string()).unwrap();

        let problems = harness_problems(
            Harness::ClaudeCode,
            &HarnessConfig::default(),
            &home,
            Probe::Fresh,
        )
        .unwrap();
        let kinds: Vec<(ProblemKind, &str)> = problems
            .iter()
            .map(|p| (p.kind, p.message.as_str()))
            .collect();
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert_eq!(kinds[0].0, ProblemKind::StatusHooksMissing);
        assert!(
            kinds[0]
                .1
                .starts_with("pm status hooks (StopFailure, Notification) not installed"),
            "{kinds:?}"
        );
        assert_eq!(kinds[1].0, ProblemKind::StatusHooksMissing);
        assert!(
            kinds[1]
                .1
                .starts_with("pm dialog hook (PermissionRequest) not installed"),
            "{kinds:?}"
        );

        let problems = team_problems(
            &AgentsConfig::default(),
            &AgentsConfig::default(),
            &HarnessConfig::default(),
            &projected_main(dir.path()),
            &home,
            &team(&["implementer"]),
        )
        .unwrap();
        assert_eq!(problems, TeamProblems::default());
    }

    #[test]
    fn missing_hooks_and_an_unknown_harness_are_problems_of_the_members_they_affect() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("empty-home");
        let mut agents = AgentsConfig::default();
        agents.harness.insert("reviewer".into(), "aider".into());

        let problems = team_problems(
            &agents,
            &AgentsConfig::default(),
            &HarnessConfig::default(),
            &projected_main(dir.path()),
            &home,
            &team(&["implementer", "reviewer"]),
        )
        .unwrap();
        let lines = problems.lines;
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("implementer (claude-code): pm hooks not installed in "),
            "{lines:?}"
        );
        assert_eq!(
            lines[1],
            "reviewer: harness 'aider' is not supported yet; supported: claude-code, codex, \
             opencode"
        );
    }

    #[test]
    fn rows_the_harness_would_refuse_at_spawn_are_problems() {
        let dir = tempdir().unwrap();
        let home = home_with_hooks(dir.path());
        let config = opencode_reporting(dir.path(), "opencode v2.0.18");
        let mut agents = mixed_agents();
        agents.models.insert("qa".into(), "qwen".into());
        agents.permissions.insert("qa".into(), "plan".into());
        // Passed through unvalidated on the other harnesses.
        agents
            .harness
            .insert("reviewer".into(), "claude-code".into());
        agents.permissions.insert("reviewer".into(), "plan".into());

        let problems = team_problems(
            &agents,
            &AgentsConfig::default(),
            &config,
            &projected_main(dir.path()),
            &home,
            &team(&["reviewer", "qa"]),
        )
        .unwrap();
        assert_eq!(problems.failing, 1);
        assert_eq!(problems.lines.len(), 2, "{:?}", problems.lines);
        assert!(
            problems.lines[0].starts_with(
                "qa (opencode): [agents.models] row for an opencode agent must be \
                 `<provider>/<model>[#variant]`; got: qwen"
            ),
            "{:?}",
            problems.lines
        );
        assert!(
            problems.lines[1].starts_with(
                "qa (opencode): [agents.permissions] row for an opencode agent must be a JSON \
                 array"
            ),
            "{:?}",
            problems.lines
        );
    }

    #[test]
    fn a_definition_the_harness_would_not_find_is_a_problem_of_its_member() {
        let dir = tempdir().unwrap();
        let home = home_with_hooks(dir.path());
        let config = opencode_reporting(dir.path(), "opencode v2.0.18");
        let mut agents = mixed_agents();
        agents.models.insert("qa".into(), "local/qwen".into());
        agents.models.insert("plain".into(), "local/qwen".into());
        agents.harness.insert("plain".into(), "opencode".into());
        agents.harness.insert("reviewer".into(), "opencode".into());
        agents.models.insert("reviewer".into(), "local/qwen".into());
        let main = dir.path().join("main");
        let check = |members: &[&str]| {
            team_problems(
                &agents,
                &AgentsConfig::default(),
                &config,
                &main,
                &home,
                &team(members),
            )
            .unwrap()
        };

        // Launched without its role; the vanilla name has no definition.
        assert_eq!(
            check(&["qa", "plain"]).lines,
            [
                "qa (opencode): definition 'qa' is not projected for opencode, so the agent would \
              start without its role (run `pm upgrade`)"
            ]
        );

        // Either tier's projection is found.
        let project = main.join(".opencode/agents");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("qa.md"), "# stub").unwrap();
        let global = Harness::OpenCode
            .global_config_dir(&home)
            .unwrap()
            .join("agents");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("reviewer.md"), "# stub").unwrap();
        assert_eq!(check(&["qa", "reviewer"]), TeamProblems::default());
    }

    #[test]
    fn a_harness_has_agents_when_a_row_or_agents_without_one_land_on_it() {
        let none = AgentsConfig::default();
        assert!(has_agents(Harness::ClaudeCode, &none, &none));
        assert!(!has_agents(Harness::Codex, &none, &none));

        let mut global = AgentsConfig::default();
        global.harness.insert("*".into(), "codex".into());
        assert!(!has_agents(Harness::ClaudeCode, &none, &global));
        assert!(has_agents(Harness::Codex, &none, &global));

        let mut project = AgentsConfig::default();
        project
            .harness
            .insert("reviewer".into(), "claude-code".into());
        assert!(has_agents(Harness::ClaudeCode, &project, &global));
        // Masking the wildcard puts agents without a row back on the default.
        let mut project = AgentsConfig::default();
        project.harness.insert("*".into(), String::new());
        assert!(has_agents(Harness::ClaudeCode, &project, &global));
        assert!(!has_agents(Harness::Codex, &project, &global));
    }

    #[test]
    fn a_model_row_bound_to_another_harness_is_named_as_the_reason() {
        let dir = tempdir().unwrap();
        let home = home_with_hooks(dir.path());
        let config = opencode_reporting(dir.path(), "opencode v2.0.18");
        let mut agents = mixed_agents();
        agents.models.insert("*".into(), "opus".into());

        let problems = team_problems(
            &agents,
            &AgentsConfig::default(),
            &config,
            &projected_main(dir.path()),
            &home,
            &team(&["qa"]),
        )
        .unwrap();
        assert_eq!(
            problems.lines[0],
            "qa (opencode): no [agents.models] row, which opencode agents need (project \
             [agents.models] row for '*' is bound to claude-code, not opencode — not applied); \
             set `[agents.models] qa = \"<provider>/<model>\"`"
        );
    }
}
