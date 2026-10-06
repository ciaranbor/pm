//! `[harness.*]`: per-harness settings with no per-agent shape.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::state::paths;

use super::config::is_default;
use super::{GlobalConfig, ProjectConfig};

/// Per-harness settings that have no per-agent shape. Present in both
/// config tiers; see `resolve_harness_config`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessConfig {
    #[serde(default, skip_serializing_if = "is_default")]
    pub codex: CodexConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub opencode: OpenCodeConfig,
}

/// `[harness.opencode]`: settings every opencode agent shares.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenCodeConfig {
    /// Pass `--auto`, approving whatever no permission rule denies; unset
    /// means `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    /// The opencode executable; unset means `opencode` from `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// `[harness.opencode.providers.<id>]`: provider entries in opencode's
    /// own schema, rendered into every opencode agent's config.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub providers: std::collections::BTreeMap<String, toml::Table>,
}

/// `[harness.codex]`: how codex agents are sandboxed. Every value is in
/// codex's own terms and passed through unvalidated.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexConfig {
    /// `-s <mode>` for agents with no `[agents.permissions]` row; unset means
    /// `danger-full-access` (pm's default — see the harness docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// `-a <policy>`; unset means `never`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    /// Extra `--add-dir` roots for a sandboxed agent, absolute or relative to
    /// the project root; a project `[]` masks the global list. pm's own
    /// state paths are always added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writable_roots: Option<Vec<String>>,
    /// Pass `--dangerously-bypass-hook-trust` instead of relying on the
    /// per-machine interactive hook trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass_hook_trust: Option<bool>,
}

/// Resolve the per-harness settings across the two tiers, per key: a set
/// project value wins; an empty string (or, for a list, `[]`) masks the
/// global one. A provider entry is one key: the project's replaces the
/// global one of the same id whole, and an empty project table masks it.
pub fn resolve_harness_config(project: &HarnessConfig, global: &HarnessConfig) -> HarnessConfig {
    let (p, g) = (&project.codex, &global.codex);
    let (po, go) = (&project.opencode, &global.opencode);
    let mut providers = go.providers.clone();
    providers.extend(po.providers.clone());
    providers.retain(|_, entry| !entry.is_empty());
    HarnessConfig {
        opencode: OpenCodeConfig {
            auto: po.auto.or(go.auto),
            binary: layered_opt(&po.binary, &go.binary),
            providers,
        },
        codex: CodexConfig {
            sandbox: layered_opt(&p.sandbox, &g.sandbox),
            approval: layered_opt(&p.approval, &g.approval),
            writable_roots: p
                .writable_roots
                .clone()
                .or_else(|| g.writable_roots.clone()),
            bypass_hook_trust: p.bypass_hook_trust.or(g.bypass_hook_trust),
        },
    }
}

/// The `[harness.*]` settings in effect for `project_root`: the project's
/// over `global`, or `global` alone outside a project and for a project
/// whose config cannot be read.
pub fn harness_config_in(project_root: Option<&Path>, global: &HarnessConfig) -> HarnessConfig {
    let project = project_root
        .and_then(|root| ProjectConfig::load(&paths::pm_dir(root)).ok())
        .map(|config| config.harness)
        .unwrap_or_default();
    resolve_harness_config(&project, global)
}

/// [`harness_config_in`] over the global config as it stands. Advisory, so
/// an unreadable project config yields the global settings.
pub fn harness_config(project_root: Option<&Path>) -> HarnessConfig {
    harness_config_in(project_root, &GlobalConfig::load_or_default().harness)
}

fn layered_opt(project: &Option<String>, global: &Option<String>) -> Option<String> {
    project
        .as_ref()
        .or(global.as_ref())
        .filter(|v| !v.is_empty())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn saved_config_writes_only_the_harness_tables_that_are_set() {
        let dir = tempdir().unwrap();
        let mut config: ProjectConfig = toml::from_str("[project]\nname = \"x\"\n").unwrap();
        config.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!text.contains("[harness"), "{text}");

        config.harness.codex.sandbox = Some("workspace-write".to_string());
        config.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(text.contains("[harness.codex]"), "{text}");
        assert!(!text.contains("[harness.opencode]"), "{text}");
        assert_eq!(ProjectConfig::load(dir.path()).unwrap(), config);
    }

    #[test]
    fn harness_codex_config_roundtrips_and_layers_project_over_global() {
        let project: ProjectConfig = toml::from_str(
            r#"
[project]
name = "myapp"

[harness.codex]
sandbox = ""
writable_roots = ["main/target"]
bypass_hook_trust = true
"#,
        )
        .unwrap();
        let global: GlobalConfig = toml::from_str(
            r#"
[harness.codex]
sandbox = "workspace-write"
approval = "on-request"
writable_roots = ["/global/cache"]
"#,
        )
        .unwrap();
        let serialized = toml::to_string_pretty(&project).unwrap();
        assert_eq!(
            toml::from_str::<ProjectConfig>(&serialized).unwrap(),
            project
        );

        let resolved = resolve_harness_config(&project.harness, &global.harness);
        assert_eq!(
            resolved.codex,
            CodexConfig {
                // "" in the project masks the global sandbox.
                sandbox: None,
                approval: Some("on-request".into()),
                writable_roots: Some(vec!["main/target".into()]),
                bypass_hook_trust: Some(true),
            }
        );
        // An empty project list masks the global roots.
        let masked: ProjectConfig =
            toml::from_str("[project]\nname = \"x\"\n[harness.codex]\nwritable_roots = []\n")
                .unwrap();
        assert_eq!(
            resolve_harness_config(&masked.harness, &global.harness)
                .codex
                .writable_roots,
            Some(Vec::new())
        );
        assert_eq!(
            resolve_harness_config(&HarnessConfig::default(), &global.harness).codex,
            global.harness.codex
        );
        // Absent everywhere: every codex default applies.
        assert_eq!(
            resolve_harness_config(&HarnessConfig::default(), &HarnessConfig::default()),
            HarnessConfig::default()
        );
    }

    #[test]
    fn opencode_providers_layer_whole_entries_project_over_global() {
        let global: GlobalConfig = toml::from_str(
            r#"
[harness.opencode.providers.local]
package = "@opencode/ai/providers/openai-compatible"
env = ["LOCAL_API_KEY"]
settings = { baseURL = "http://127.0.0.1:8000/v1" }

[harness.opencode.providers.hosted]
package = "pkg"

[harness.opencode.providers.shared]
package = "shared-pkg"
"#,
        )
        .unwrap();
        let project: ProjectConfig = toml::from_str(
            r#"
[project]
name = "myapp"

[harness.opencode.providers.local]
package = "other"

[harness.opencode.providers.hosted]

[harness.opencode.providers.mine]
package = "mine-pkg"
"#,
        )
        .unwrap();

        let resolved = resolve_harness_config(&project.harness, &global.harness).opencode;
        let table = |text: &str| text.parse::<toml::Table>().unwrap();
        assert_eq!(
            resolved.providers,
            [
                // Replaced whole: the global entry's `env` and `settings` are gone.
                ("local".to_string(), table("package = \"other\"")),
                ("mine".to_string(), table("package = \"mine-pkg\"")),
                ("shared".to_string(), table("package = \"shared-pkg\"")),
            ]
            .into()
        );

        assert_eq!(
            resolve_harness_config(&HarnessConfig::default(), &global.harness)
                .opencode
                .providers,
            global.harness.opencode.providers
        );
    }

    #[test]
    fn config_without_providers_serializes_no_providers_table() {
        let config: ProjectConfig = toml::from_str("[project]\nname = \"x\"\n").unwrap();
        let written = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<ProjectConfig>(&written).unwrap(), config);
        assert!(!written.contains("providers"), "{written}");
    }
}
