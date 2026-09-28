//! `[harness.opencode.providers.<id>]`: provider entries pm renders into an
//! opencode agent's config.
//!
//! An entry is opencode's own `providers.<id>` object written as TOML. pm
//! converts it to JSON as written and interprets two things only:
//!
//! - **Secrets.** pm config is committed and synced, so a key is named,
//!   never stored: `env = ["NAME"]`, or `{env:NAME}` where opencode
//!   substitutes it. An `apiKey` that is anything but exactly `{env:NAME}`,
//!   or an `Authorization` header with no `{env:…}` in it, refuses the spawn.
//!   opencode sends the request without a key when the variable is unset, so
//!   an unset one is reported, but only as a remark: the agent's environment
//!   is the tmux server's and its shell's, which pm's own only approximates.
//! - **`models`.** opencode resolves no model a provider does not list, so
//!   the model of the agent's row is added to its provider's list.
//!
//! An entry replaces, whole, a provider of the same id from opencode's own
//! config files. So pm renders nothing for a provider pm config does not
//! define: the agent's row only enables it, and a definition in the
//! user-level `opencode.json` keeps serving it.
//!
//! opencode drops a key it does not know silently, and drops the **whole
//! entry** when a known field has the wrong type; the rest of the config,
//! the restriction included, still applies. Only opencode knows its schema,
//! so [`config_issues`] asks it what it kept. The same answer lists every
//! other config document opencode merges in — the user-level
//! `opencode.json`, a `.opencode/` directory above the worktree — and a
//! provider restriction in one of those replaces the one pm writes.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use super::{CONFIG_ENV, ModelRef, api_command};
use crate::error::{PmError, Result};
use crate::state::project::OpenCodeConfig;

type Providers = BTreeMap<String, toml::Table>;

/// The `providers` object of an agent's config: every entry as written,
/// plus `model` in the list of its provider when pm defines that provider.
pub(super) fn render(
    providers: &Providers,
    model: Option<&ModelRef<'_>>,
) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for (id, entry) in providers {
        let mut rendered = Map::new();
        for (key, value) in entry {
            rendered.insert(key.clone(), to_json(id, key, key, value)?);
        }
        if let Some(model) = model.filter(|model| model.provider == id) {
            let models = rendered
                .entry("models")
                .or_insert_with(|| Value::Object(Map::new()));
            let Value::Object(models) = models else {
                return Err(invalid(id, "models", "must be a table of model ids"));
            };
            models
                .entry(model.id)
                .or_insert_with(|| Value::Object(Map::new()));
        }
        out.insert(id.clone(), Value::Object(rendered));
    }
    Ok(out)
}

/// The providers an agent may use: the one its row names and every one pm
/// config defines, which is what lets a subagent run on another of them.
pub(super) fn enabled<'a>(providers: &'a Providers, model: &ModelRef<'a>) -> Vec<&'a str> {
    let mut ids: Vec<&str> = providers.keys().map(String::as_str).collect();
    ids.push(model.provider);
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn invalid(id: &str, path: &str, problem: &str) -> PmError {
    PmError::Agent(format!(
        "[harness.opencode.providers.{id}] `{path}` {problem}"
    ))
}

/// `value`, held under `key` at `path` of provider `id`, as JSON, refusing a
/// secret. The error names where the value is, never what it is.
fn to_json(id: &str, path: &str, key: &str, value: &toml::Value) -> Result<Value> {
    let named = |text: Option<&str>, ok: fn(&str) -> bool| text.is_some_and(ok);
    if key.eq_ignore_ascii_case("apiKey") && !named(value.as_str(), is_env_reference) {
        return Err(invalid(
            id,
            path,
            "must name the variable holding the key, as `{env:NAME}` — or drop it and set \
             `env = [\"NAME\"]`; pm config never holds the key itself",
        ));
    }
    if key.eq_ignore_ascii_case("Authorization")
        && !named(value.as_str(), |text| !env_references(text).is_empty())
    {
        return Err(invalid(
            id,
            path,
            "must take its credential from a variable, as in `Bearer {env:NAME}` — or drop it \
             and set `env = [\"NAME\"]`; pm config never holds the credential itself",
        ));
    }
    Ok(match value {
        toml::Value::String(text) => Value::String(text.clone()),
        toml::Value::Integer(number) => Value::from(*number),
        toml::Value::Boolean(flag) => Value::Bool(*flag),
        toml::Value::Float(number) => serde_json::Number::from_f64(*number)
            .map(Value::Number)
            .ok_or_else(|| invalid(id, path, "is a number JSON cannot hold"))?,
        toml::Value::Datetime(_) => {
            return Err(invalid(
                id,
                path,
                "is a TOML datetime, which JSON cannot hold; quote it",
            ));
        }
        toml::Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| to_json(id, path, key, item))
                .collect::<Result<_>>()?,
        ),
        toml::Value::Table(table) => Value::Object(
            table
                .iter()
                .map(|(key, item)| {
                    let at = format!("{path}.{key}");
                    Ok((key.clone(), to_json(id, &at, key, item)?))
                })
                .collect::<Result<_>>()?,
        ),
    })
}

fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `text` is exactly `{env:NAME}`.
fn is_env_reference(text: &str) -> bool {
    text.strip_prefix("{env:")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(is_env_name)
}

/// Every `NAME` of an `{env:NAME}` in `text`.
fn env_references(text: &str) -> Vec<&str> {
    text.split("{env:")
        .skip(1)
        .filter_map(|rest| rest.split_once('}'))
        .map(|(name, _)| name)
        .filter(|name| is_env_name(name))
        .collect()
}

/// One remark per provider whose key cannot be found by `is_set`: no
/// variable of its `env` list (any one of them serves), or one an
/// `{env:NAME}` names.
pub(super) fn unset_key_notes<'a>(
    providers: impl IntoIterator<Item = (&'a String, &'a toml::Table)>,
    is_set: impl Fn(&str) -> bool,
) -> Vec<String> {
    fn references<'a>(value: &'a toml::Value, out: &mut Vec<&'a str>) {
        match value {
            toml::Value::String(text) => out.extend(env_references(text)),
            toml::Value::Array(items) => items.iter().for_each(|item| references(item, out)),
            toml::Value::Table(table) => table.values().for_each(|item| references(item, out)),
            _ => {}
        }
    }

    let mut notes = Vec::new();
    for (id, entry) in providers {
        let listed: Vec<&str> = entry
            .get("env")
            .and_then(toml::Value::as_array)
            .map(|names| names.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        let mut unset = Vec::new();
        if !listed.iter().any(|name| is_set(name)) {
            unset.extend(listed);
        }
        let mut referenced = Vec::new();
        entry
            .values()
            .for_each(|value| references(value, &mut referenced));
        unset.extend(referenced.into_iter().filter(|name| !is_set(name)));
        unset.sort_unstable();
        unset.dedup();
        if !unset.is_empty() {
            notes.push(format!(
                "opencode provider '{id}' takes its key from {}, unset in pm's environment; \
                 unless the agent's own environment sets it, requests go out without a key",
                unset.join(", ")
            ));
        }
    }
    notes
}

/// Whether pm's own environment holds a value for `name`.
pub(super) fn set_in_environment(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// What is wrong with the configured providers, for `pm doctor`: an entry
/// pm refuses to render, one opencode dropped, a provider restriction from
/// another config document opencode merges in for an agent started in
/// `worktree`, and keys that look unset.
pub(super) fn config_issues(cfg: &OpenCodeConfig, worktree: &Path) -> Vec<String> {
    let mut issues = Vec::new();
    let mut rendered = Map::new();
    for (id, entry) in &cfg.providers {
        let one = Providers::from([(id.clone(), entry.clone())]);
        match render(&one, None) {
            Ok(entry) => rendered.extend(entry),
            Err(PmError::Agent(message)) => issues.push(message),
            Err(e) => issues.push(e.to_string()),
        }
    }
    issues.extend(merged_config_issues(cfg, worktree, rendered).unwrap_or_default());
    issues.extend(unset_key_notes(&cfg.providers, set_in_environment));
    issues
}

/// What opencode's own reading of the config shows. `None` when opencode
/// cannot be asked or answers something else than its config documents.
fn merged_config_issues(
    cfg: &OpenCodeConfig,
    worktree: &Path,
    rendered: Map<String, Value>,
) -> Option<Vec<String>> {
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

    let out = api_command(cfg, &["config.get"])
        .env(CONFIG_ENV, file.path())
        .current_dir(worktree)
        .output()
        .ok()?;
    let response: Value = serde_json::from_slice(&out.stdout).ok()?;
    let documents = response
        .get("data")
        .unwrap_or(&response)
        .as_array()?
        .iter()
        .filter(|source| source["type"] == "document");

    let own = file.path().canonicalize().ok()?;
    let mut issues = Vec::new();
    for document in documents {
        let Some(path) = document["path"].as_str().map(Path::new) else {
            continue;
        };
        let info = &document["info"];
        if path.canonicalize().is_ok_and(|path| path == own) {
            for id in &ids {
                if info["providers"].get(id).is_none() {
                    issues.push(format!(
                        "opencode dropped [harness.opencode.providers.{id}]: a field of it has \
                         a type opencode's provider schema does not accept"
                    ));
                }
            }
        } else if restricts_providers(info) {
            issues.push(format!(
                "{} restricts providers (`enabled_providers` or a `provider.use` policy); \
                 opencode applies it in place of the restriction pm writes, so pm's agents \
                 fail with `Model unavailable` or reach providers pm config does not name — \
                 remove it",
                crate::path_utils::to_portable(path)
            ));
        }
    }
    Some(issues)
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
    use serde_json::json;

    fn providers(text: &str) -> Providers {
        #[derive(serde::Deserialize)]
        struct Config {
            providers: Providers,
        }
        toml::from_str::<Config>(text).unwrap().providers
    }

    fn model(row: &str) -> ModelRef<'_> {
        ModelRef::parse(row).unwrap()
    }

    const LOCAL: &str = r#"
[providers.local]
name = "Local"
package = "@opencode/ai/providers/openai-compatible"
env = ["LOCAL_API_KEY", "OTHER_KEY"]
settings = { baseURL = "http://127.0.0.1:8000/v1", timeout = 30, stream = true, scale = 0.5 }

[providers.local.models."mlx-community/Qwen3.8-27B-4bit"]
name = "Qwen"
limit = { context = 32768 }

[providers.hosted]
package = "pkg"
settings = { apiKey = "{env:HOSTED_KEY}" }
headers = { Authorization = "Bearer {env:HOSTED_TOKEN}", "X-Team" = "pm" }
"#;

    #[test]
    fn entries_are_rendered_as_written_with_the_rows_model_listed() {
        let rendered = render(&providers(LOCAL), Some(&model("local/other-model#high"))).unwrap();
        assert_eq!(
            Value::Object(rendered),
            json!({
                "local": {
                    "name": "Local",
                    "package": "@opencode/ai/providers/openai-compatible",
                    "env": ["LOCAL_API_KEY", "OTHER_KEY"],
                    "settings": {
                        "baseURL": "http://127.0.0.1:8000/v1",
                        "timeout": 30,
                        "stream": true,
                        "scale": 0.5,
                    },
                    "models": {
                        "mlx-community/Qwen3.8-27B-4bit": {
                            "name": "Qwen",
                            "limit": {"context": 32768},
                        },
                        "other-model": {},
                    },
                },
                "hosted": {
                    "package": "pkg",
                    "settings": {"apiKey": "{env:HOSTED_KEY}"},
                    "headers": {
                        "Authorization": "Bearer {env:HOSTED_TOKEN}",
                        "X-Team": "pm",
                    },
                },
            })
        );
    }

    #[test]
    fn a_listed_model_keeps_its_own_settings_and_a_bare_entry_gets_a_list() {
        let listed = render(
            &providers(LOCAL),
            Some(&model("local/mlx-community/Qwen3.8-27B-4bit")),
        )
        .unwrap();
        assert_eq!(
            listed["local"]["models"],
            json!({"mlx-community/Qwen3.8-27B-4bit": {
                "name": "Qwen",
                "limit": {"context": 32768},
            }})
        );

        let bare = render(
            &providers("[providers.local]\npackage = \"pkg\"\n"),
            Some(&model("local/qwen")),
        )
        .unwrap();
        assert_eq!(
            Value::Object(bare),
            json!({"local": {"package": "pkg", "models": {"qwen": {}}}})
        );
    }

    #[test]
    fn a_row_on_a_provider_pm_does_not_define_is_enabled_not_defined() {
        let all = providers(LOCAL);
        let row = model("anthropic/claude-opus-5");
        assert_eq!(enabled(&all, &row), ["anthropic", "hosted", "local"]);
        let rendered = render(&all, Some(&row)).unwrap();
        assert_eq!(rendered.keys().collect::<Vec<_>>(), ["hosted", "local"]);
        assert_eq!(
            enabled(&all, &model("local/qwen")),
            ["hosted", "local"],
            "the row's provider is listed once"
        );
    }

    #[test]
    fn a_stored_secret_is_refused_without_repeating_it() {
        for (entry, at) in [
            (
                r#"settings = { apiKey = "sk-live-123" }"#,
                "settings.apiKey",
            ),
            (
                r#"settings = { apiKey = "Bearer {env:KEY}" }"#,
                "settings.apiKey",
            ),
            (r#"settings = { apiKey = "{env:}" }"#, "settings.apiKey"),
            (r#"settings = { apiKey = 123 }"#, "settings.apiKey"),
            (
                r#"models = { qwen = { settings = { apikey = "sk-live-123" } } }"#,
                "models.qwen.settings.apikey",
            ),
            (
                r#"headers = { Authorization = "Bearer sk-live-123" }"#,
                "headers.Authorization",
            ),
            (
                r#"models = { qwen = { headers = { authorization = "sk-live-123" } } }"#,
                "models.qwen.headers.authorization",
            ),
        ] {
            let all = providers(&format!("[providers.local]\n{entry}\n"));
            let err = render(&all, None).unwrap_err().to_string();
            assert!(
                err.contains(&format!("[harness.opencode.providers.local] `{at}` must")),
                "{entry}: {err}"
            );
            assert!(err.contains("env = [\"NAME\"]"), "{entry}: {err}");
            assert!(!err.contains("sk-live"), "{entry}: {err}");
        }
    }

    #[test]
    fn a_value_json_cannot_hold_is_refused() {
        for (entry, problem) in [
            ("settings = { since = 2026-01-01 }", "TOML datetime"),
            ("settings = { scale = nan }", "number JSON cannot hold"),
            ("models = [\"qwen\"]", "`models` must be a table"),
        ] {
            let all = providers(&format!("[providers.local]\n{entry}\n"));
            let err = render(&all, Some(&model("local/qwen")))
                .unwrap_err()
                .to_string();
            assert!(err.contains(problem), "{entry}: {err}");
        }
    }

    #[test]
    fn a_key_no_variable_holds_is_remarked_on() {
        let all = providers(LOCAL);
        assert_eq!(unset_key_notes(&all, |_| true), Vec::<String>::new());
        // Any one variable of an `env` list serves; every `{env:…}` is needed.
        assert_eq!(
            unset_key_notes(&all, |name| name == "OTHER_KEY" || name == "HOSTED_KEY"),
            [
                "opencode provider 'hosted' takes its key from HOSTED_TOKEN, unset in pm's \
                 environment; unless the agent's own environment sets it, requests go out \
                 without a key"
            ]
        );
        let notes = unset_key_notes(&all, |_| false);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("from HOSTED_KEY, HOSTED_TOKEN,"),
            "{notes:?}"
        );
        assert!(
            notes[1].contains("from LOCAL_API_KEY, OTHER_KEY,"),
            "{notes:?}"
        );
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
            config_issues(&cfg, dir.path()),
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
        assert_eq!(config_issues(&cfg, dir.path()), Vec::<String>::new());

        cfg.providers = providers("[providers.local]\nsettings = { apiKey = \"sk-live-123\" }\n");
        let issues = config_issues(&cfg, dir.path());
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(
            issues[0].starts_with("[harness.opencode.providers.local] `settings.apiKey` must"),
            "{issues:?}"
        );
    }
}
