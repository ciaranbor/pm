//! `[agents.*]` and `[harness.*]` config: rows a spawn would refuse or
//! remark on, legacy `default` rows, and which harnesses each worktree needs.

use std::path::{Path, PathBuf};

use super::{Fix, Issue, IssueKind};
use crate::commands::harness_check;
use crate::commands::{agent_spawn, skills};
use crate::error::Result;
use crate::harness::{ConfigIssueKind, Harness};
use crate::state::agent::AgentRegistry;
use crate::state::feature::FeatureState;
use crate::state::project::{
    AgentsConfig, CodexConfig, GlobalConfig, ProjectConfig, WILDCARD_AGENT, harness_config,
    resolve_agent_settings,
};
use crate::state::workflow::{self, LEGACY_VANILLA_AGENT, VANILLA_AGENT};
use crate::state::{paths, scope};

/// Main-scope findings about what each harness in use is configured with:
/// agents it will refuse for want of a model row or remarks on at spawn,
/// and whatever the harness itself reports about its `[harness.<name>]`
/// settings.
pub(super) fn harness_config_issues(project_root: &Path) -> Result<Vec<Issue>> {
    let (project, global) = agents_configs(project_root)?;
    let mut definitions: Vec<String> = project
        .harness
        .keys()
        .chain(global.harness.keys())
        .filter(|key| *key != WILDCARD_AGENT)
        .cloned()
        .collect();
    for (_, launched) in worktree_definitions(project_root)? {
        definitions.extend(launched);
    }
    definitions.sort();
    definitions.dedup();

    let config = harness_config(Some(project_root));
    let mut issues = Vec::new();
    let mut rows: Vec<(Harness, String)> = Vec::new();
    // The catch-all row reaches agents no other row names.
    if let Ok(settings) = resolve_agent_settings(&project, &global, WILDCARD_AGENT)
        && let Some(model) = settings.model
    {
        rows.push((settings.harness, model));
    }
    for definition in definitions {
        let Ok(settings) = resolve_agent_settings(&project, &global, &definition) else {
            continue;
        };
        if let Some(model) = &settings.model {
            rows.push((settings.harness, model.clone()));
        }
        for note in settings
            .harness
            .row_notes(&config, settings.model.as_deref())
        {
            issues.push(Issue {
                kind: IssueKind::AgentRowRemark,
                message: format!("agent '{definition}': {note}"),
                fix: Fix::None,
            });
        }
        if let Some(dropped) = harness_check::missing_model_row(&settings) {
            issues.push(Issue {
                kind: IssueKind::AgentModelMissing,
                message: format!(
                    "agent '{definition}' runs on {} and has no [agents.models] row, so it \
                     will not spawn{dropped}",
                    settings.harness
                ),
                fix: Fix::None,
            });
        }
        for issue in harness_check::row_issues(&settings) {
            issues.push(Issue {
                kind: IssueKind::AgentRowInvalid,
                message: format!("agent '{definition}' will not spawn: {issue}"),
                fix: Fix::None,
            });
        }
    }

    let main = paths::main_worktree(project_root);
    for harness in skills::harnesses_in_use(project_root)? {
        let rows: Vec<String> = rows
            .iter()
            .filter(|(on, _)| *on == harness)
            .map(|(_, row)| row.clone())
            .collect();
        for issue in harness.config_issues(&config, &main, &rows) {
            issues.push(Issue {
                kind: match issue.kind {
                    ConfigIssueKind::Invalid => IssueKind::HarnessConfigInvalid,
                    ConfigIssueKind::KeyUnset => IssueKind::ProviderKeyUnset,
                    ConfigIssueKind::ProviderUnreachable => IssueKind::ProviderUnreachable,
                },
                message: issue.message,
                fix: Fix::None,
            });
        }
    }
    Ok(issues)
}

/// Keys pm does not read where they are set: one a harness no longer
/// reads, in either config file, or a global-only one in the project's.
/// Config loading drops or ignores them silently.
pub(super) fn unread_key_issues(project_root: &Path) -> Vec<Issue> {
    let mut files = vec![(
        Tier::Project,
        paths::pm_dir(project_root).join("config.toml"),
    )];
    if let Ok(dir) = paths::global_config_dir() {
        files.push((Tier::Global, dir.join("config.toml")));
    }
    unread_key_issues_in(&files)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    Project,
    Global,
}

fn unread_key_issues_in(files: &[(Tier, PathBuf)]) -> Vec<Issue> {
    let retired = Harness::SUPPORTED
        .iter()
        .flat_map(|harness| harness.retired_config_keys())
        .map(|(key, instead)| {
            (
                key.to_string(),
                format!("which pm no longer reads; {instead}"),
                None,
            )
        });
    let global_only = CodexConfig::GLOBAL_ONLY.iter().map(|name| {
        let key = format!("harness.codex.{name}");
        let instead = format!(
            "which pm reads from the global config only; set it with `pm config set --global \
             {key} <value>`"
        );
        (key, instead, Some(Tier::Project))
    });
    let keys: Vec<(String, String, Option<Tier>)> = retired.chain(global_only).collect();
    let mut issues = Vec::new();
    for (tier, path) in files {
        let Some(table) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
        else {
            continue;
        };
        for (key, why, only_in) in &keys {
            if only_in.is_some_and(|only| only != *tier) || !has_key(&table, key) {
                continue;
            }
            let tier = match tier {
                Tier::Project => "project",
                Tier::Global => "global",
            };
            issues.push(Issue {
                kind: IssueKind::UnreadConfigKey,
                message: format!(
                    "{tier} config {} sets `{key}`, {why}",
                    crate::path_utils::to_portable(path)
                ),
                fix: Fix::None,
            });
        }
    }
    issues
}

/// Whether `table` holds the dotted `key`.
fn has_key(table: &toml::Table, key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    let Some((leaf, parents)) = parts.split_last() else {
        return false;
    };
    parents
        .iter()
        .try_fold(table, |t, part| t.get(*part)?.as_table())
        .is_some_and(|t| t.contains_key(*leaf))
}

/// The `[agents.*]` tables of the two tiers.
pub(super) fn agents_configs(project_root: &Path) -> Result<(AgentsConfig, AgentsConfig)> {
    let project = match ProjectConfig::load(&paths::pm_dir(project_root)) {
        Ok(config) => config.agents,
        Err(crate::error::PmError::NotInProject) => Default::default(),
        Err(e) => return Err(e),
    };
    Ok((project, GlobalConfig::load_or_default().agents))
}

/// Main-scope findings for each installed workflow, in either tier, whose
/// `config.toml` fails to load: no feature can start on it.
pub(super) fn workflow_config_issues(project_root: &Path) -> Result<Vec<Issue>> {
    let global = workflow::global_dir()?;
    let installed = workflow::list_installed_in(Some(project_root), &global)?;
    Ok(installed
        .errors
        .into_iter()
        // A directory without a `config.toml` is no workflow to load.
        .filter(|(name, _)| workflow::resolve_dir(Some(project_root), name, &global).is_some())
        .map(|(name, error)| {
            let mut message = format!("workflow '{name}' fails to load: {error}");
            if name == scope::MAIN_WORKFLOW {
                message.push_str(&format!(
                    " (until it loads, pm takes main's orchestrator to be '{}')",
                    scope::DEFAULT_ORCHESTRATOR
                ));
            }
            Issue {
                kind: IssueKind::WorkflowConfigInvalid,
                message,
                fix: Fix::None,
            }
        })
        .collect())
}

/// Each worktree on disk with the definitions launched in it: those of its
/// registered agents plus, for a feature, its workflow team.
fn worktree_definitions(project_root: &Path) -> Result<Vec<(PathBuf, Vec<String>)>> {
    let agents_dir = paths::agents_dir(project_root);
    let features_dir = paths::features_dir(project_root);
    let mut out = Vec::new();
    for (scope, wt) in skills::scoped_worktrees_on_disk(project_root)? {
        let registry = AgentRegistry::load(&agents_dir, &scope)?;
        let mut definitions: Vec<String> = registry
            .agents
            .iter()
            .map(|(key, entry)| entry.effective_definition(key).to_string())
            .collect();
        if let Ok(feature) = FeatureState::load(&features_dir, &scope)
            && let Some(name) = &feature.workflow
            && let Ok(def) = workflow::WorkflowDef::load(project_root, name)
        {
            definitions.extend(def.agents.iter().cloned());
        }
        out.push((wt, definitions));
    }
    Ok(out)
}

/// Each worktree on disk with the harnesses its agents launch on — the set
/// a spawn there would need the worktree trusted by.
pub(super) fn worktree_harnesses(project_root: &Path) -> Result<Vec<(PathBuf, Vec<Harness>)>> {
    let (project, global) = agents_configs(project_root)?;
    let mut out = Vec::new();
    for (wt, definitions) in worktree_definitions(project_root)? {
        let mut harnesses = Vec::new();
        for def in definitions {
            if let Ok(h) = agent_spawn::configured_harness(&def, &project, &global)
                && !harnesses.contains(&h)
            {
                harnesses.push(h);
            }
        }
        out.push((wt, harnesses));
    }
    Ok(out)
}

/// One warning per `[agents.*]` row keyed `default` while no `default`
/// definition exists: the row matches nothing, and was likely meant as `"*"`
/// (every agent) or `plain` (the vanilla agent).
pub(super) fn legacy_vanilla_row_issues(project_root: &Path) -> Result<Vec<Issue>> {
    if workflow::definition_exists(
        project_root,
        LEGACY_VANILLA_AGENT,
        paths::home_dir().ok().as_deref(),
    ) {
        return Ok(Vec::new());
    }
    let (project, global) = agents_configs(project_root)?;
    let mut issues = Vec::new();
    for (tier, config) in [("project", &project), ("global", &global)] {
        for (table, rows) in [
            ("harness", &config.harness),
            ("models", &config.models),
            ("permissions", &config.permissions),
        ] {
            if rows.contains_key(LEGACY_VANILLA_AGENT) {
                issues.push(Issue {
                    kind: IssueKind::LegacyVanillaConfigRow,
                    message: format!(
                        "{tier} [agents.{table}] row '{LEGACY_VANILLA_AGENT}' matches no agent \
                         definition: use \"{WILDCARD_AGENT}\" for every agent or \
                         '{VANILLA_AGENT}' for the vanilla agent"
                    ),
                    fix: Fix::None,
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
    use crate::state::agent::AgentRegistry;
    use crate::testing::{TestServer, agent_entry};
    use tempfile::tempdir;

    #[test]
    fn opencode_agents_without_a_model_row_and_refused_providers_are_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        assert!(harness_config_issues(&project_path).unwrap().is_empty());

        use_opencode(&project_path, "*", "opencode v2.0.23");
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        let agents = &mut config.agents;
        for (definition, harness) in [("reviewer", "opencode"), ("planner", "claude-code")] {
            agents.harness.insert(definition.into(), harness.into());
        }
        agents.models.insert("reviewer".into(), "local/qwen".into());
        config.harness.opencode.providers = [(
            "local".to_string(),
            "settings = { apiKey = \"sk-live-123\" }".parse().unwrap(),
        )]
        .into();
        config.save(&pm_dir).unwrap();
        // Launched in a worktree, on the harness `*` names, with no row.
        let mut registry = AgentRegistry::default();
        registry.register(
            "frontend-dev",
            crate::state::agent::AgentEntry {
                active: false,
                agent_definition: Some("implementer".to_string()),
                harness: Harness::OpenCode,
                ..agent_entry("frontend-dev")
            },
        );
        registry
            .save(&paths::agents_dir(&project_path), "main")
            .unwrap();

        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentModelMissing),
            [
                "agent 'implementer' runs on opencode and has no [agents.models] row, so it will \
              not spawn"
            ]
        );
        let invalid = messages(&issues, IssueKind::HarnessConfigInvalid);
        assert_eq!(invalid.len(), 1, "{invalid:?}");
        assert!(
            invalid[0].starts_with("[harness.opencode.providers.local] `settings.apiKey` must"),
            "{invalid:?}"
        );
        assert!(!invalid[0].contains("sk-live"), "{invalid:?}");

        // A row that exists but is bound to another harness.
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        let agents = &mut config.agents;
        agents.harness.insert("*".into(), "claude-code".into());
        agents.models.remove("reviewer");
        agents.models.insert("*".into(), "opus".into());
        config.save(&pm_dir).unwrap();
        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentModelMissing),
            [
                "agent 'reviewer' runs on opencode and has no [agents.models] row, so it will \
              not spawn (project [agents.models] row for '*' is bound to claude-code, not \
              opencode — not applied)"
            ]
        );
    }

    #[test]
    fn a_retired_or_misplaced_key_is_reported() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project.toml");
        let global = dir.path().join("global.toml");
        std::fs::write(&project, "[harness.codex]\nsandbox = \"read-only\"\n").unwrap();
        std::fs::write(&global, "[harness.codex]\nbypass_hook_trust = true\n").unwrap();
        let files = [
            (Tier::Project, project.clone()),
            (Tier::Global, global.clone()),
        ];

        let issues = unread_key_issues_in(&files);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind(), IssueKind::UnreadConfigKey);
        assert!(!issues[0].auto_fixable());
        let message = issues[0].message();
        assert!(
            message.starts_with(&format!(
                "global config {} sets `harness.codex.bypass_hook_trust`, which pm no longer \
                 reads",
                global.display()
            )),
            "{message}"
        );
        assert!(
            message.contains("`pm config set --global harness.codex.trust_pm_hooks true`"),
            "{message}"
        );

        // A global-only key: read from the global file, not the project's.
        std::fs::write(&global, "[harness.codex]\ntrust_pm_hooks = true\n").unwrap();
        assert!(unread_key_issues_in(&files).is_empty());
        std::fs::write(&project, "[harness.codex]\ntrust_pm_hooks = true\n").unwrap();
        let issues = unread_key_issues_in(&files);
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].message().starts_with(&format!(
                "project config {} sets `harness.codex.trust_pm_hooks`, which pm reads from the \
                 global config only",
                project.display()
            )),
            "{}",
            issues[0].message()
        );
    }

    #[test]
    fn opencode_rows_a_spawn_would_refuse_are_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        use_opencode(&project_path, "reviewer", "opencode v2.0.23");
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .models
            .insert("reviewer".into(), "qwen".into());
        config.save(&pm_dir).unwrap();

        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentRowInvalid),
            [
                "agent 'reviewer' will not spawn: [agents.models] row for an opencode agent \
                 must be `<provider>/<model>[#variant]`; got: qwen"
            ]
        );
    }

    #[test]
    fn an_opencode_model_its_provider_does_not_declare_is_reported() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        use_opencode(&project_path, "reviewer", "opencode v2.0.23");
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .models
            .insert("reviewer".into(), "local/qwen-typo".into());
        config.harness.opencode.providers = [(
            "local".to_string(),
            "models = { qwen = {} }".parse().unwrap(),
        )]
        .into();
        config.save(&pm_dir).unwrap();

        let issues = harness_config_issues(&project_path).unwrap();
        assert_eq!(
            messages(&issues, IssueKind::AgentRowRemark),
            ["agent 'reviewer': model 'qwen-typo' is not among those \
                 [harness.opencode.providers.local] declares (qwen); if it is a typo, every turn \
                 fails at the endpoint"]
        );
    }

    #[test]
    fn config_row_keyed_default_is_flagged_until_a_default_definition_exists() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let pm_dir = paths::pm_dir(&project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .permissions
            .insert("default".into(), "auto".into());
        config.agents.models.insert("plain".into(), "opus".into());
        config.save(&pm_dir).unwrap();

        assert_eq!(
            messages(
                &legacy_vanilla_row_issues(&project_path).unwrap(),
                IssueKind::LegacyVanillaConfigRow
            ),
            [
                "project [agents.permissions] row 'default' matches no agent definition: use \
                 \"*\" for every agent or 'plain' for the vanilla agent"
            ]
        );

        let defs = paths::main_worktree(&project_path).join(".agents/agents");
        std::fs::create_dir_all(&defs).unwrap();
        std::fs::write(defs.join("default.md"), "stub").unwrap();
        assert!(legacy_vanilla_row_issues(&project_path).unwrap().is_empty());
    }

    #[test]
    fn a_workflow_whose_config_fails_to_load_is_reported_naming_its_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        let workflows = paths::workflows_dir(&project_path);
        let write = |name: &str, body: &str| {
            std::fs::create_dir_all(workflows.join(name)).unwrap();
            std::fs::write(workflows.join(name).join("config.toml"), body).unwrap();
        };
        write("good", "description = \"ok\"\n");
        write("broken", "description = \"unclosed\n");
        write(
            "old",
            "description = \"ok\"\nauto_spawn = [\"implementer\"]\n",
        );
        write(scope::MAIN_WORKFLOW, "agents = [\"supervisor\"]\n");
        std::fs::create_dir_all(workflows.join("notes-only")).unwrap();

        let found = messages(
            &workflow_config_issues(&project_path).unwrap(),
            IssueKind::WorkflowConfigInvalid,
        );
        let config = |name: &str| workflows.join(name).join("config.toml");
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            found[0]
                == format!(
                    "workflow 'broken' fails to load: {}: invalid basic string, expected `\"` \
                     (line 1, column 24)",
                    config("broken").display()
                ),
            "{found:?}"
        );
        assert!(
            found[1].starts_with(&format!(
                "workflow 'old' fails to load: {} sets `auto_spawn`",
                config("old").display()
            )),
            "{found:?}"
        );
        assert!(
            found[2].starts_with(&format!(
                "workflow 'orchestrator' fails to load: {}: ",
                config("orchestrator").display()
            )) && found[2].ends_with("(until it loads, pm takes main's orchestrator to be 'main')"),
            "{found:?}"
        );
        assert_eq!(scope::orchestrator_definition(&project_path), "main");
    }
}
