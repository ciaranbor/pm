//! What `pm doctor` reports about `[harness.opencode.providers]`: entries
//! pm refuses to render ([`super::providers`]), and what only opencode's own
//! reading of its merged config shows — an entry it dropped, a provider
//! restriction elsewhere that replaces pm's, and providers out of pm's
//! agents' reach ([`super::reach`]) — plus keys that look unset.

use std::path::Path;

use serde_json::{Map, Value};

use super::api::{CALL, command};
use super::providers::{
    Providers, render, set_in_environment, split_model_id_notes, unset_key_notes,
};
use super::reach::Reach;
use super::{CONFIG_ENV, ModelRef};
use crate::bounded;
use crate::harness::{ConfigIssue, ConfigIssueKind};
use crate::state::project::OpenCodeConfig;

/// What is wrong with the configured providers, for `pm doctor`: an entry
/// pm refuses to render, one opencode dropped, a provider restriction from
/// another config document opencode merges in for an agent started in
/// `worktree`, providers pm's agents need or may expect but cannot reach,
/// and keys that look unset. `rows` are the opencode agents' model rows.
pub(super) fn config_issues(
    cfg: &OpenCodeConfig,
    worktree: &Path,
    rows: &[String],
) -> Vec<ConfigIssue> {
    let issue = |kind| move |message| ConfigIssue { kind, message };
    let mut invalid = Vec::new();
    let mut rendered = Map::new();
    for (id, entry) in &cfg.providers {
        let one = Providers::from([(id.clone(), entry.clone())]);
        match render(&one, None) {
            Ok(entry) => rendered.extend(entry),
            Err(e) => invalid.push(e.reason()),
        }
    }
    invalid.extend(split_model_id_notes(&cfg.providers));
    let row_providers: Vec<&str> = rows
        .iter()
        .filter_map(|row| ModelRef::parse(row).ok())
        .map(|model| model.provider)
        .collect();
    let merged = merged_config_issues(cfg, worktree, rendered, &row_providers).unwrap_or_default();
    invalid.extend(merged.invalid);

    let mut issues: Vec<ConfigIssue> = invalid
        .into_iter()
        .map(issue(ConfigIssueKind::Invalid))
        .collect();
    issues.extend(
        merged
            .unreachable
            .into_iter()
            .map(issue(ConfigIssueKind::ProviderUnreachable)),
    );
    issues.extend(
        unset_key_notes(&cfg.providers, set_in_environment)
            .into_iter()
            .map(issue(ConfigIssueKind::KeyUnset)),
    );
    issues
}

/// What opencode's own reading of the config shows.
#[derive(Default)]
struct Merged {
    invalid: Vec<String>,
    unreachable: Vec<String>,
}

/// [`Merged`] for the config documents and directories opencode reads in
/// `worktree`. `None` when opencode cannot be asked or answers something
/// else than its config sources.
fn merged_config_issues(
    cfg: &OpenCodeConfig,
    worktree: &Path,
    rendered: Map<String, Value>,
    row_providers: &[&str],
) -> Option<Merged> {
    let file = tempfile::Builder::new()
        .prefix("pm-opencode-doctor-")
        .suffix(".json")
        .tempfile()
        .ok()?;
    let ids: Vec<String> = rendered.keys().cloned().collect();
    let config = Value::Object(Map::from_iter([(
        "providers".to_string(),
        Value::Object(rendered),
    )]));
    std::fs::write(file.path(), config.to_string()).ok()?;

    let mut command = command(cfg, &["api"], &["config.get"]);
    command.env(CONFIG_ENV, file.path()).current_dir(worktree);
    let out = match bounded::run(&mut command, CALL) {
        Ok(out) => out,
        Err(failure @ bounded::Failure::TimedOut { .. }) => {
            return Some(Merged {
                invalid: vec![format!(
                    "{}, so pm could not check what opencode made of [harness.opencode]",
                    failure.describe("opencode config.get")
                )],
                ..Default::default()
            });
        }
        Err(bounded::Failure::Unrunnable { .. }) => return None,
    };
    let response: Value = serde_json::from_slice(&out.stdout).ok()?;
    let sources = response.get("data").unwrap_or(&response).as_array()?;

    let own = file.path().canonicalize().ok()?;
    let defined: Vec<&str> = cfg.providers.keys().map(String::as_str).collect();
    let reach = Reach {
        defined: &defined,
        rows: row_providers,
    };
    let mut merged = Merged::default();
    for source in sources {
        let Some(path) = source["path"].as_str().map(Path::new) else {
            continue;
        };
        if source["type"] == "directory" {
            merged
                .unreachable
                .extend(reach.unreachable_definitions(path));
            continue;
        }
        if source["type"] != "document" {
            continue;
        }
        let info = &source["info"];
        if path.canonicalize().is_ok_and(|path| path == own) {
            for id in &ids {
                if info["providers"].get(id).is_none() {
                    merged.invalid.push(format!(
                        "opencode dropped [harness.opencode.providers.{id}]: a field of it has \
                         a type opencode's provider schema does not accept"
                    ));
                }
            }
            continue;
        }
        let shown = crate::path_utils::to_portable(path);
        if restricts_providers(info) {
            merged.invalid.push(format!(
                "{shown} restricts providers (`enabled_providers` or a `provider.use` policy); \
                 opencode applies it in place of the restriction pm writes, so pm's agents \
                 fail with `Model unavailable` or reach providers pm config does not name — \
                 remove it"
            ));
        }
        merged
            .unreachable
            .extend(reach.unreachable_providers(info, &shown));
    }
    Some(merged)
}

/// opencode reports `enabled_providers` as the policies it becomes.
fn restricts_providers(info: &Value) -> bool {
    info.pointer("/experimental/policies")
        .and_then(Value::as_array)
        .is_some_and(|policies| {
            policies
                .iter()
                .any(|policy| policy["action"] == "provider.use")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn providers(text: &str) -> Providers {
        #[derive(serde::Deserialize)]
        struct Config {
            providers: Providers,
        }
        toml::from_str::<Config>(text).unwrap().providers
    }

    /// An opencode that answers `config.get` with `documents`, naming pm's
    /// own file by the path it was handed.
    fn opencode_answering(dir: &Path, documents: &str) -> OpenCodeConfig {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("opencode");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nsed \"s|@OWN@|$OPENCODE_CONFIG|\" <<'ANSWER'\n{documents}\nANSWER\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        OpenCodeConfig {
            binary: Some(bin.to_string_lossy().into_owned()),
            providers: providers(
                "[providers.local]\npackage = \"pkg\"\n[providers.typo]\npackage = 5\n",
            ),
            ..Default::default()
        }
    }

    fn texts(issues: Vec<ConfigIssue>, kind: ConfigIssueKind) -> Vec<String> {
        issues
            .into_iter()
            .filter(|issue| issue.kind == kind)
            .map(|issue| issue.message)
            .collect()
    }

    #[test]
    fn doctor_reports_providers_pm_agents_cannot_reach() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        std::fs::create_dir_all(user.join("agents")).unwrap();
        for (name, model) in [
            ("helper", "model: userlocal/qwen"),
            ("rowed", "model: \"hosted/big\""),
            ("defined", "model: local/qwen"),
            ("plain", "description: no model"),
        ] {
            std::fs::write(
                user.join(format!("agents/{name}.md")),
                format!("---\n{model}\nmode: subagent\n---\nmodel: ignored/body\n"),
            )
            .unwrap();
        }
        // The shape opencode 2.0.23 answers with.
        let cfg = opencode_answering(
            dir.path(),
            &format!(
                r#"[
                  {{"type":"document","path":"/home/u/.config/opencode/opencode.json","info":{{
                    "providers":{{"userlocal":{{"package":"pkg"}},"hosted":{{"package":"pkg"}},
                                  "local":{{"package":"pkg"}}}}}}}},
                  {{"type":"directory","path":"{}"}},
                  {{"type":"document","path":"@OWN@","info":{{"providers":{{"local":{{}}}}}}}}
                ]"#,
                user.display()
            ),
        );
        let unreachable = texts(
            config_issues(&cfg, dir.path(), &["hosted/big".to_string()]),
            ConfigIssueKind::ProviderUnreachable,
        );
        assert_eq!(unreachable.len(), 3, "{unreachable:?}");
        assert!(
            unreachable[1].starts_with(&format!(
                "opencode definition {}/agents/helper.md runs on 'userlocal/qwen', and pm \
                 config does not define provider 'userlocal', so no pm agent can run it",
                user.display()
            )),
            "{unreachable:?}"
        );
        assert!(
            unreachable[2].contains(
                "rowed.md runs on 'hosted/big', and pm config does not define provider \
                 'hosted', so only an agent whose own [agents.models] row is on 'hosted' can \
                 run it"
            ),
            "{unreachable:?}"
        );
        assert_eq!(
            unreachable[0],
            "opencode provider 'userlocal' is defined only in \
             /home/u/.config/opencode/opencode.json, so no pm agent or subagent can use it: an \
             opencode agent reaches only the providers pm config defines and the one its own \
             [agents.models] row names; define it as [harness.opencode.providers.userlocal] to \
             make it reachable"
        );
    }

    #[test]
    fn an_unset_key_is_advisory() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = opencode_answering(dir.path(), "[]");
        cfg.providers = providers("[providers.local]\nenv = [\"PM_TEST_KEY_NOBODY_SETS\"]\n");
        let issues = config_issues(&cfg, dir.path(), &[]);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].kind, ConfigIssueKind::KeyUnset);
    }

    #[test]
    fn doctor_reports_what_opencode_dropped_and_what_overrides_pm() {
        let dir = tempfile::tempdir().unwrap();
        // The shape opencode 2.0.18 answers with.
        let cfg = opencode_answering(
            dir.path(),
            r#"[
              {"type":"document","path":"/home/u/.config/opencode/opencode.jsonc","info":{
                "experimental":{"policies":[
                  {"action":"provider.use","resource":"*","effect":"deny"},
                  {"action":"provider.use","resource":"anthropic","effect":"allow"}]}}},
              {"type":"directory","path":"/home/u/.config/opencode"},
              {"type":"document","path":"@OWN@","info":{"providers":{"local":{"package":"pkg"}}}},
              {"type":"document","path":"/proj/.opencode/opencode.json","info":{"model":"a/b"}}
            ]"#,
        );
        assert_eq!(
            texts(
                config_issues(&cfg, dir.path(), &[]),
                ConfigIssueKind::Invalid
            ),
            [
                "/home/u/.config/opencode/opencode.jsonc restricts providers \
                 (`enabled_providers` or a `provider.use` policy); opencode applies it in place \
                 of the restriction pm writes, so pm's agents fail with `Model unavailable` or \
                 reach providers pm config does not name — remove it",
                "opencode dropped [harness.opencode.providers.typo]: a field of it has a type \
                 opencode's provider schema does not accept",
            ]
        );
    }

    #[test]
    fn doctor_reports_an_entry_pm_refuses_and_nothing_opencode_did_not_say() {
        let dir = tempfile::tempdir().unwrap();
        // What a call answers when it is not a list of config documents.
        let mut cfg = opencode_answering(dir.path(), r#"{"data":{"id":"ses_1"}}"#);
        assert_eq!(config_issues(&cfg, dir.path(), &[]), []);

        cfg.providers = providers("[providers.local]\nsettings = { apiKey = \"sk-live-123\" }\n");
        let issues = texts(
            config_issues(&cfg, dir.path(), &[]),
            ConfigIssueKind::Invalid,
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(
            issues[0].starts_with("[harness.opencode.providers.local] `settings.apiKey` must"),
            "{issues:?}"
        );
    }
}
