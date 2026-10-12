//! The per-spawn config file, and the agent's `[agents.models]` and
//! `[agents.permissions]` rows as opencode takes them.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::providers;
use crate::error::{PmError, Result};
use crate::harness::SpawnSpec;
use crate::harness::spawn::glob_literal;
use crate::state::project::OpenCodeConfig;

/// An `[agents.models]` row, as opencode's session API takes it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModelRef<'a> {
    pub(super) provider: &'a str,
    pub(super) id: &'a str,
    pub(super) variant: Option<&'a str>,
}

impl<'a> ModelRef<'a> {
    /// `<provider>/<model>[#variant]`. The model id may itself hold `/`.
    pub(super) fn parse(row: &'a str) -> Result<Self> {
        let (model, variant) = match row.split_once('#') {
            Some((model, variant)) => (model, Some(variant)),
            None => (row, None),
        };
        match model.split_once('/') {
            Some((provider, id))
                if !provider.is_empty() && !id.is_empty() && variant != Some("") =>
            {
                Ok(Self {
                    provider,
                    id,
                    variant,
                })
            }
            _ => Err(PmError::Agent(format!(
                "[agents.models] row for an opencode agent must be \
                 `<provider>/<model>[#variant]`; got: {row}"
            ))),
        }
    }

    pub(super) fn to_json(&self) -> Value {
        let mut out = json!({"providerID": self.provider, "id": self.id});
        if let Some(variant) = self.variant {
            out["variant"] = json!(variant);
        }
        out
    }
}

/// What is worth remarking on about a model row that a spawn still takes.
pub(in crate::harness) fn row_notes(cfg: &OpenCodeConfig, model: Option<&str>) -> Vec<String> {
    model
        .and_then(|row| ModelRef::parse(row).ok())
        .and_then(|model| providers::undeclared_model_note(&cfg.providers, &model))
        .into_iter()
        .collect()
}

/// An `[agents.permissions]` row: opencode's own rule list.
fn parse_permission_rules(row: &str) -> Result<Vec<Value>> {
    match serde_json::from_str::<Value>(row) {
        Ok(Value::Array(rules)) => Ok(rules),
        _ => Err(PmError::Agent(format!(
            "[agents.permissions] row for an opencode agent must be a JSON array of \
             opencode permission rules, e.g. \
             '[{{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"deny\"}}]'; got: {row}"
        ))),
    }
}

/// What a spawn would refuse about an agent's rows.
pub(in crate::harness) fn row_issues(
    model: Option<&str>,
    permission_mode: Option<&str>,
) -> Vec<String> {
    let model = model.and_then(|row| ModelRef::parse(row).err());
    let permissions = permission_mode.and_then(|row| parse_permission_rules(row).err());
    model
        .into_iter()
        .chain(permissions)
        .map(PmError::reason)
        .collect()
}

/// Each protected dir a rule can name, spelled as given and, where that
/// differs, symlinks resolved: opencode matches a path outside the session
/// dir lexically, in whichever spelling the tool was handed.
fn protected_spellings(protected_dirs: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for dir in protected_dirs {
        let resolved = dir.canonicalize().ok();
        for spelling in std::iter::once(dir).chain(resolved.as_ref()) {
            if let Some(text) = glob_literal(spelling)
                && !out.contains(&text)
            {
                out.push(text);
            }
        }
    }
    out
}

/// The per-spawn config.
pub(super) fn render_config(
    spec: &SpawnSpec<'_>,
    cfg: &OpenCodeConfig,
    row: &str,
    model: &ModelRef<'_>,
) -> Result<Value> {
    let mut config = serde_json::Map::new();
    config.insert("model".to_string(), json!(row));
    config.insert(
        "enabled_providers".to_string(),
        json!(providers::enabled(&cfg.providers, model)),
    );
    let defined = providers::render(&cfg.providers, Some(model))?;
    if !defined.is_empty() {
        config.insert("providers".to_string(), Value::Object(defined));
    }

    let mut rules = Vec::new();
    if cfg.auto == Some(false) {
        for dir in spec.writable_dirs {
            rules.push(json!({
                "action": "external_directory",
                "resource": format!("{}/*", dir.display()),
                "effect": "allow",
            }));
        }
    }
    if let Some(row) = spec.permission_mode {
        rules.extend(parse_permission_rules(row)?);
    }
    // Last match wins, so these follow the row, which cannot undo them.
    for dir in protected_spellings(spec.protected_dirs) {
        rules.push(json!({
            "action": "edit",
            "resource": format!("{dir}/*"),
            "effect": "deny",
        }));
    }
    if !rules.is_empty() {
        config.insert("permissions".to_string(), Value::Array(rules));
    }

    Ok(Value::Object(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`render_config`] for `spec` on the row `local/qwen`, less what the
    /// row itself puts there.
    fn rendered(spec: &SpawnSpec<'_>, cfg: &OpenCodeConfig) -> Result<Value> {
        let row = "local/qwen";
        let mut config = render_config(spec, cfg, row, &ModelRef::parse(row)?)?;
        let own = config.as_object_mut().unwrap();
        assert_eq!(own.remove("model"), Some(json!(row)));
        assert_eq!(own.remove("enabled_providers"), Some(json!(["local"])));
        Ok(config)
    }

    #[test]
    fn render_config_sets_no_rules_without_a_permissions_row_under_auto() {
        let dirs = vec![PathBuf::from("/proj/.pm")];
        let spec = SpawnSpec {
            writable_dirs: &dirs,
            ..Default::default()
        };
        assert_eq!(
            rendered(&spec, &OpenCodeConfig::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn render_config_passes_the_permission_row_through_as_rules() {
        let spec = SpawnSpec {
            permission_mode: Some(
                r#"[{"action":"edit","resource":"*","effect":"deny"},{"action":"made-up"}]"#,
            ),
            ..Default::default()
        };
        assert_eq!(
            rendered(&spec, &OpenCodeConfig::default()).unwrap(),
            json!({
                "permissions": [
                    {"action": "edit", "resource": "*", "effect": "deny"},
                    {"action": "made-up"},
                ],
            })
        );
    }

    #[test]
    fn render_config_without_auto_allows_pm_state_dirs_ahead_of_the_row() {
        let dirs = vec![PathBuf::from("/proj/.pm"), PathBuf::from("/proj/main/.git")];
        let spec = SpawnSpec {
            permission_mode: Some(
                r#"[{"action":"external_directory","resource":"*","effect":"deny"}]"#,
            ),
            writable_dirs: &dirs,
            ..Default::default()
        };
        let off = OpenCodeConfig {
            auto: Some(false),
            ..Default::default()
        };
        // Last match wins in opencode, so the row can still override pm's
        // allows.
        assert_eq!(
            rendered(&spec, &off).unwrap(),
            json!({"permissions": [
                {"action": "external_directory", "resource": "/proj/.pm/*", "effect": "allow"},
                {"action": "external_directory", "resource": "/proj/main/.git/*", "effect": "allow"},
                {"action": "external_directory", "resource": "*", "effect": "deny"},
            ]})
        );
    }

    #[test]
    fn protected_dirs_are_denied_after_the_row_with_or_without_auto() {
        let protected = vec![PathBuf::from("/proj/main")];
        let spec = SpawnSpec {
            permission_mode: Some(
                r#"[{"action":"edit","resource":"/proj/main/*","effect":"allow"}]"#,
            ),
            protected_dirs: &protected,
            ..Default::default()
        };
        let expected = json!({"permissions": [
            {"action": "edit", "resource": "/proj/main/*", "effect": "allow"},
            {"action": "edit", "resource": "/proj/main/*", "effect": "deny"},
        ]});
        for auto in [None, Some(true)] {
            let cfg = OpenCodeConfig {
                auto,
                ..Default::default()
            };
            assert_eq!(rendered(&spec, &cfg).unwrap(), expected, "{auto:?}");
        }
    }

    #[test]
    fn a_symlinked_protected_dir_is_denied_in_both_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = real.with_file_name("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let protected = vec![link.clone()];
        let spec = SpawnSpec {
            protected_dirs: &protected,
            ..Default::default()
        };
        let resources: Vec<Value> =
            rendered(&spec, &OpenCodeConfig::default()).unwrap()["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|rule| rule["resource"].clone())
                .collect();
        assert_eq!(
            resources,
            [
                json!(format!("{}/*", link.display())),
                json!(format!("{}/*", real.display())),
            ]
        );
    }

    #[test]
    fn render_config_rejects_a_row_that_is_not_a_rule_list() {
        for row in ["acceptEdits", r#"{"action":"edit"}"#, "[unterminated"] {
            let spec = SpawnSpec {
                permission_mode: Some(row),
                ..Default::default()
            };
            let err = rendered(&spec, &OpenCodeConfig::default())
                .unwrap_err()
                .to_string();
            assert!(err.contains("must be a JSON array"), "{row}: {err}");
            assert!(err.ends_with(&format!("got: {row}")), "{row}: {err}");
        }
    }
}
