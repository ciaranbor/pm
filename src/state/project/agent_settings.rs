//! Per-agent spawn settings, layered across the two config tiers.

use crate::error::Result;
use crate::harness::Harness;

use super::AgentsConfig;

/// Per-agent spawn settings resolved across the two config tiers.
/// `None` means pm emits no flag — the agent inherits the harness's default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentSettings {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub harness: Harness,
    /// One line per configured row the resolution dropped, for the spawn
    /// message.
    pub notes: Vec<String>,
    /// The one of `notes` about the model row, when that was dropped.
    pub dropped_model: Option<String>,
}

type Rows = std::collections::BTreeMap<String, String>;

/// Resolve one agent definition's settings: the project entry wins over the
/// global one, per agent and per key. An empty project value masks a set
/// global value ("inherit the default here"). Errors when the effective
/// harness names one pm can't spawn — never silently falls back.
///
/// A model or permission row is in its harness's own vocabulary, which pm
/// never validates, so each row is bound to the harness configured under the
/// key it matched, looked up from the row's own tier down: a named row to
/// that tier's named (else `"*"`) harness row, else the tier below's, else
/// the default; a `"*"` row to the `"*"` harness row the same way. A row
/// bound to a harness other than the one the agent spawns on is dropped with
/// a note rather than falling through.
pub fn resolve_agent_settings(
    project: &AgentsConfig,
    global: &AgentsConfig,
    definition: &str,
) -> Result<AgentSettings> {
    let harness = layered(&project.harness, &global.harness, definition)
        .map(|h| h.parse::<Harness>())
        .transpose()?
        .unwrap_or_default();

    let resolve = |table: &str, project_rows: &Rows, global_rows: &Rows| {
        let tiers: [(&str, &Rows, &[&Rows]); 2] = [
            (
                "project",
                project_rows,
                &[&project.harness, &global.harness],
            ),
            ("global", global_rows, &[&global.harness]),
        ];
        for (tier, rows, harness_tiers) in tiers {
            let Some((key, value)) = tier_row(rows, definition) else {
                continue;
            };
            if value.is_empty() {
                return (None, None);
            }
            let (bound, bound_name) = bound_harness(harness_tiers, key);
            if bound == Some(harness) {
                return (Some(value.clone()), None);
            }
            let dropped = format!(
                "{tier} [agents.{table}] row for '{key}' is bound to {bound_name}, not \
                 {harness} — not applied"
            );
            return (None, Some(dropped));
        }
        (None, None)
    };
    let (permission_mode, dropped_permissions) =
        resolve("permissions", &project.permissions, &global.permissions);
    let (model, dropped_model) = resolve("models", &project.models, &global.models);
    Ok(AgentSettings {
        permission_mode,
        model,
        harness,
        notes: dropped_permissions
            .into_iter()
            .chain(dropped_model.clone())
            .collect(),
        dropped_model,
    })
}

/// The harness the given harness tiers configure under `key` (first tier
/// with a row wins, `""` masks to the default), and its name for a report.
/// An unparseable value only errors when it is the effective harness; here
/// it is a harness no row can be bound to.
fn bound_harness(tiers: &[&Rows], key: &str) -> (Option<Harness>, String) {
    match tiers.iter().find_map(|rows| tier_row(rows, key)) {
        Some((_, raw)) if !raw.is_empty() => (raw.parse::<Harness>().ok(), raw.clone()),
        _ => (Some(Harness::default()), Harness::default().to_string()),
    }
}

/// The `[agents.*]` row key that applies to every agent without a row of its
/// own. Agent names are limited to `[A-Za-z0-9_-]`, so it can never name one.
pub const WILDCARD_AGENT: &str = "*";

/// The row one tier holds for `key`: the named row, else the wildcard row.
/// Returns the key that matched so a report can name it.
fn tier_row<'a>(
    rows: &'a std::collections::BTreeMap<String, String>,
    key: &str,
) -> Option<(&'a str, &'a String)> {
    rows.get_key_value(key)
        .or_else(|| rows.get_key_value(WILDCARD_AGENT))
        .map(|(k, v)| (k.as_str(), v))
}

/// One per-agent key across the two tiers: project named row, project
/// wildcard, global named row, global wildcard; the first present wins and
/// `""` masks.
pub(crate) fn layered(
    project: &std::collections::BTreeMap<String, String>,
    global: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Option<String> {
    tier_row(project, key)
        .or_else(|| tier_row(global, key))
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PmError;

    fn agents_config(permissions: &[(&str, &str)], models: &[(&str, &str)]) -> AgentsConfig {
        let map = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        AgentsConfig {
            permissions: map(permissions),
            models: map(models),
            harness: Default::default(),
        }
    }

    #[test]
    fn agent_settings_project_overrides_global_per_agent() {
        let project = agents_config(
            &[("implementer", "acceptEdits")],
            &[("implementer", "opus")],
        );
        let global = agents_config(
            &[("implementer", "plan"), ("reviewer", "plan")],
            &[("implementer", "sonnet"), ("reviewer", "fable")],
        );

        let implementer = resolve_agent_settings(&project, &global, "implementer").unwrap();
        assert_eq!(implementer.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(implementer.model.as_deref(), Some("opus"));

        // Absent from the project map entirely — falls through to global.
        let reviewer = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(reviewer.permission_mode.as_deref(), Some("plan"));
        assert_eq!(reviewer.model.as_deref(), Some("fable"));
    }

    #[test]
    fn agent_settings_unset_at_both_tiers_is_none() {
        let settings = resolve_agent_settings(
            &AgentsConfig::default(),
            &AgentsConfig::default(),
            "researcher",
        )
        .unwrap();
        assert_eq!(settings, AgentSettings::default());
    }

    #[test]
    fn agent_settings_empty_project_value_masks_global() {
        // An explicit "" in the project config means "inherit Claude's
        // default here", not "fall through to the global value".
        let project = agents_config(&[("reviewer", "")], &[("reviewer", "")]);
        let global = agents_config(&[("reviewer", "plan")], &[("reviewer", "opus")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings, AgentSettings::default());
    }

    #[test]
    fn agent_settings_empty_global_value_is_none() {
        let global = agents_config(&[("reviewer", "")], &[("reviewer", "")]);
        let settings =
            resolve_agent_settings(&AgentsConfig::default(), &global, "reviewer").unwrap();
        assert_eq!(settings, AgentSettings::default());
    }

    #[test]
    fn agent_settings_resolve_for_vanilla_agent() {
        // `plain` is only filtered out of the `--agent` flag; it is still a
        // normal lookup key for per-agent settings.
        let global = agents_config(&[], &[("plain", "opus")]);
        let settings = resolve_agent_settings(&AgentsConfig::default(), &global, "plain").unwrap();
        assert_eq!(settings.model.as_deref(), Some("opus"));
    }

    #[test]
    fn agent_settings_harness_layers_project_over_global_per_key() {
        let mut project = AgentsConfig::default();
        project
            .harness
            .insert("implementer".to_string(), "claude-code".to_string());
        let mut global = AgentsConfig::default();
        global
            .harness
            .insert("implementer".to_string(), "opencode".to_string());
        global
            .harness
            .insert("reviewer".to_string(), "claude-code".to_string());

        // Project row masks the (unsupported) global row for the same key.
        let implementer = resolve_agent_settings(&project, &global, "implementer").unwrap();
        assert_eq!(implementer.harness, Harness::ClaudeCode);
        // Absent from the project map — falls through to global.
        let reviewer = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(reviewer.harness, Harness::ClaudeCode);
        // Unset at both tiers — the default.
        let researcher = resolve_agent_settings(&project, &global, "researcher").unwrap();
        assert_eq!(researcher.harness, Harness::ClaudeCode);
    }

    fn with_harness(mut config: AgentsConfig, rows: &[(&str, &str)]) -> AgentsConfig {
        for (k, v) in rows {
            config.harness.insert(k.to_string(), v.to_string());
        }
        config
    }

    #[test]
    fn agent_settings_global_rows_drop_when_project_moves_agent_to_another_harness() {
        // A global Claude Code model must not reach a codex agent as `-m opus`.
        let project = with_harness(AgentsConfig::default(), &[("reviewer", "codex")]);
        let global = agents_config(&[("reviewer", "plan")], &[("reviewer", "opus")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.harness, Harness::Codex);
        assert_eq!(settings.permission_mode, None);
        assert_eq!(settings.model, None);
        assert_eq!(
            settings.notes,
            vec![
                "global [agents.permissions] row for 'reviewer' is bound to claude-code, not \
                 codex — not applied",
                "global [agents.models] row for 'reviewer' is bound to claude-code, not codex \
                 — not applied",
            ]
        );
    }

    #[test]
    fn agent_settings_global_rows_apply_next_to_a_global_harness_row() {
        let global = with_harness(
            agents_config(&[("reviewer", "workspace-write")], &[("reviewer", "gpt-5")]),
            &[("reviewer", "codex")],
        );
        let settings =
            resolve_agent_settings(&AgentsConfig::default(), &global, "reviewer").unwrap();
        assert_eq!(settings.harness, Harness::Codex);
        assert_eq!(settings.permission_mode.as_deref(), Some("workspace-write"));
        assert_eq!(settings.model.as_deref(), Some("gpt-5"));
        assert!(settings.notes.is_empty());
    }

    #[test]
    fn agent_settings_project_rows_bind_through_the_project_tier_harness() {
        let project = with_harness(
            agents_config(&[("reviewer", "workspace-write")], &[("reviewer", "gpt-5")]),
            &[("reviewer", "codex")],
        );
        let global = agents_config(&[("reviewer", "plan")], &[("reviewer", "opus")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.permission_mode.as_deref(), Some("workspace-write"));
        assert_eq!(settings.model.as_deref(), Some("gpt-5"));
        assert!(settings.notes.is_empty());

        // A project harness row naming the default the global tier already
        // resolves to changes nothing: global rows still apply.
        let project = with_harness(AgentsConfig::default(), &[("reviewer", "claude-code")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.permission_mode.as_deref(), Some("plan"));
        assert_eq!(settings.model.as_deref(), Some("opus"));
        assert!(settings.notes.is_empty());
    }

    #[test]
    fn agent_settings_project_mask_of_a_mismatched_global_row_reports_nothing() {
        // The mask already dropped the row, so binding has nothing to report.
        let global = agents_config(&[("reviewer", "plan")], &[("reviewer", "opus")]);
        let project = with_harness(
            agents_config(&[("reviewer", "")], &[("reviewer", "")]),
            &[("reviewer", "codex")],
        );
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.permission_mode, None);
        assert_eq!(settings.model, None);
        assert!(settings.notes.is_empty());
    }

    #[test]
    fn agent_settings_unparseable_global_harness_overridden_by_project_drops_global_rows() {
        let project = with_harness(AgentsConfig::default(), &[("reviewer", "claude-code")]);
        let global = with_harness(
            agents_config(&[], &[("reviewer", "some-model")]),
            &[("reviewer", "opencode")],
        );
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.harness, Harness::ClaudeCode);
        assert_eq!(settings.model, None);
        assert_eq!(
            settings.notes,
            vec![
                "global [agents.models] row for 'reviewer' is bound to opencode, not \
                 claude-code — not applied"
            ]
        );
    }

    #[test]
    fn agent_settings_wildcard_row_applies_to_every_agent_below_named_rows() {
        let project = agents_config(&[("*", "acceptEdits")], &[("reviewer", "sonnet")]);
        let global = agents_config(
            &[("implementer", "plan")],
            &[("*", "opus"), ("implementer", "haiku")],
        );
        // Named project row beats the project wildcard; project wildcard beats
        // a global named row; global named row beats the global wildcard.
        let reviewer = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(reviewer.model.as_deref(), Some("sonnet"));
        assert_eq!(reviewer.permission_mode.as_deref(), Some("acceptEdits"));
        let implementer = resolve_agent_settings(&project, &global, "implementer").unwrap();
        assert_eq!(implementer.model.as_deref(), Some("haiku"));
        assert_eq!(implementer.permission_mode.as_deref(), Some("acceptEdits"));
        let researcher = resolve_agent_settings(&project, &global, "researcher").unwrap();
        assert_eq!(researcher.model.as_deref(), Some("opus"));
        assert_eq!(researcher.permission_mode.as_deref(), Some("acceptEdits"));
    }

    #[test]
    fn agent_settings_empty_wildcard_masks_the_tier_below() {
        let project = agents_config(&[], &[("*", "")]);
        let global = agents_config(&[], &[("reviewer", "opus"), ("*", "sonnet")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.model, None);
        assert!(settings.notes.is_empty());
    }

    #[test]
    fn agent_settings_wildcard_harness_binds_its_tier_rows() {
        let global = with_harness(
            agents_config(&[("*", "workspace-write")], &[("*", "gpt-5")]),
            &[("*", "codex")],
        );
        let settings =
            resolve_agent_settings(&AgentsConfig::default(), &global, "reviewer").unwrap();
        assert_eq!(settings.harness, Harness::Codex);
        assert_eq!(settings.model.as_deref(), Some("gpt-5"));
        assert_eq!(settings.permission_mode.as_deref(), Some("workspace-write"));
        assert!(settings.notes.is_empty());

        // A project row moving one agent off the wildcard harness drops the
        // global wildcard rows for it, and the note names the row that matched.
        let project = with_harness(AgentsConfig::default(), &[("reviewer", "claude-code")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.harness, Harness::ClaudeCode);
        assert_eq!(settings.model, None);
        assert_eq!(settings.permission_mode, None);
        assert_eq!(
            settings.notes,
            vec![
                "global [agents.permissions] row for '*' is bound to codex, not claude-code — \
                 not applied",
                "global [agents.models] row for '*' is bound to codex, not claude-code — not \
                 applied",
            ]
        );
    }

    #[test]
    fn agent_settings_project_row_binds_to_a_global_harness_row_for_its_key() {
        // The project tier has no harness row, so the binding lookup falls
        // through to the global one — the same lookup that picks the
        // effective harness.
        let project = agents_config(&[("reviewer", "workspace-write")], &[("reviewer", "gpt-5")]);
        let global = with_harness(AgentsConfig::default(), &[("reviewer", "codex")]);
        let settings = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(settings.harness, Harness::Codex);
        assert_eq!(settings.model.as_deref(), Some("gpt-5"));
        assert_eq!(settings.permission_mode.as_deref(), Some("workspace-write"));
        assert!(settings.notes.is_empty());
    }

    #[test]
    fn agent_settings_wildcard_row_does_not_reach_an_agent_named_onto_another_harness() {
        // A `*` model row is bound to the `*` harness (unset: claude-code),
        // so a named codex row in the same tier takes the agent out of its
        // reach.
        let project = with_harness(
            agents_config(&[], &[("*", "opus")]),
            &[("reviewer", "codex")],
        );
        let global = AgentsConfig::default();
        let reviewer = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(reviewer.harness, Harness::Codex);
        assert_eq!(reviewer.model, None);
        assert_eq!(
            reviewer.notes,
            vec![
                "project [agents.models] row for '*' is bound to claude-code, not codex — not \
                 applied"
            ]
        );
        let implementer = resolve_agent_settings(&project, &global, "implementer").unwrap();
        assert_eq!(implementer.model.as_deref(), Some("opus"));
        assert!(implementer.notes.is_empty());
    }

    #[test]
    fn agent_settings_wildcard_rows_bind_to_the_wildcard_harness_row() {
        let project = with_harness(
            agents_config(&[("*", "acceptEdits")], &[("*", "gpt-5")]),
            &[("*", "codex"), ("reviewer", "claude-code")],
        );
        let global = AgentsConfig::default();
        // The `*` rows are bound to codex, so the agent named onto claude-code
        // gets neither — whatever their values look like, pm binds by key.
        let reviewer = resolve_agent_settings(&project, &global, "reviewer").unwrap();
        assert_eq!(reviewer.harness, Harness::ClaudeCode);
        assert_eq!(reviewer.permission_mode, None);
        assert_eq!(reviewer.model, None);
        assert_eq!(reviewer.notes.len(), 2, "{:?}", reviewer.notes);
        // …and every agent on the `*` harness gets both.
        let implementer = resolve_agent_settings(&project, &global, "implementer").unwrap();
        assert_eq!(implementer.harness, Harness::Codex);
        assert_eq!(implementer.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(implementer.model.as_deref(), Some("gpt-5"));
        assert!(implementer.notes.is_empty());
    }

    #[test]
    fn agent_settings_resolve_codex() {
        let mut global = AgentsConfig::default();
        global
            .harness
            .insert("implementer".to_string(), "codex".to_string());
        let settings =
            resolve_agent_settings(&AgentsConfig::default(), &global, "implementer").unwrap();
        assert_eq!(settings.harness, Harness::Codex);
    }

    #[test]
    fn agent_settings_unsupported_harness_errors() {
        let mut global = AgentsConfig::default();
        global
            .harness
            .insert("implementer".to_string(), "aider".to_string());
        let err =
            resolve_agent_settings(&AgentsConfig::default(), &global, "implementer").unwrap_err();
        assert!(
            matches!(err, PmError::HarnessUnsupported { ref value, .. } if value == "aider"),
            "got: {err}"
        );
        assert_eq!(
            err.to_string(),
            "harness 'aider' is not supported yet; supported: claude-code, codex, opencode"
        );
        // Other agents are unaffected by a bad row they don't use.
        resolve_agent_settings(&AgentsConfig::default(), &global, "reviewer").unwrap();
    }
}
