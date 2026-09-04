//! `pm workflow show` and `pm workflow list`.
//!
//! - `show` prints the feature's active `workflow.md` plus an appended
//!   `## summary.md` brevity note (so the summary owner sees it through
//!   the command they actually run). Used by the bundled `pm-workflow`
//!   skill so agents can discover their per-feature routing at the start
//!   of every turn.
//! - `list` enumerates installed workflows across both tiers with one-line
//!   descriptions, tagged by tier and origin.

use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::workflow::{self, WorkflowDef};

/// Appended to every `pm workflow show` so the `summary.md` brevity rule
/// reaches the summary owner through the channel they actually use. Lives
/// here (not in each `workflow.md`) to keep a single source of truth
/// rather than duplicating the rule across every bundled workflow.
const SUMMARY_GUIDANCE: &str = "\
## summary.md

If the active workflow names you the summary owner, create `summary.md`
in the worktree root and keep it updated — brief and high signal-to-noise
— just what the orchestrator needs to triage, plus any succinct
out-of-scope bugs/ideas.
No exhaustive change logs or manual-test walkthroughs unless they carry
durable signal. It's collected when the feature is merged or deleted.
";

/// Resolve the active workflow for the current scope and return its
/// `workflow.md` content. Returns `Ok(None)` for scopes with no active
/// workflow (main scope, legacy features, or features created without
/// `--workflow`).
pub fn show(project_root: &Path, scope: &str) -> Result<Option<String>> {
    // Main scope has no feature state and therefore no workflow.
    if scope == "main" {
        return Ok(None);
    }
    let features_dir = paths::features_dir(project_root);
    let state = FeatureState::load(&features_dir, scope)?;
    let Some(workflow_name) = state.workflow.as_deref() else {
        return Ok(None);
    };

    let md_path = match workflow::workflow_md_path(project_root, workflow_name) {
        Some(p) if p.is_file() => p,
        Some(p) => {
            return Err(PmError::WorkflowNotFound(format!(
                "{workflow_name} (workflow.md missing at {})",
                p.display()
            )));
        }
        None => return Err(PmError::WorkflowNotFound(workflow_name.to_string())),
    };
    let mut body = std::fs::read_to_string(&md_path)?;
    // Append the summary.md brevity guidance so it reaches whoever runs
    // the command. A single blank line separates it from the workflow's
    // own prose regardless of how `workflow.md` ends.
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(SUMMARY_GUIDANCE);
    Ok(Some(body))
}

/// Output of [`list_rows`]: stdout rows (successfully-parsed workflows)
/// plus stderr warnings (workflows whose `config.toml` failed to parse).
pub struct ListOutput {
    pub rows: Vec<String>,
    pub warnings: Vec<String>,
}

/// Build the column-aligned listing used by `pm workflow list`. Returns
/// one row per installed workflow, sorted by name and tagged
/// `[<tier>, bundled|custom]` (a project entry shadows a same-named global
/// one), plus a warning per broken `config.toml` so users don't discover
/// the breakage only when `pm feat new --workflow <name>` fails. Outside a
/// project (`None`) only the global tier is listed.
pub fn list_rows(project_root: Option<&Path>) -> Result<ListOutput> {
    list_rows_in(project_root, &workflow::global_dir()?)
}

/// [`list_rows`] against an explicit global workflow tier.
pub fn list_rows_in(project_root: Option<&Path>, global_dir: &Path) -> Result<ListOutput> {
    let installed = workflow::list_installed_in(project_root, global_dir)?;

    let max_name = installed
        .workflows
        .iter()
        .map(|w| w.name.len())
        .max()
        .unwrap_or(0);
    let tags: Vec<String> = installed
        .workflows
        .iter()
        .map(|w| {
            let origin = if w.tier == workflow::Tier::Global
                && crate::commands::skills::is_bundled_workflow(&w.name)
            {
                "bundled"
            } else {
                "custom"
            };
            format!("[{}, {origin}]", w.tier)
        })
        .collect();
    let max_tag = tags.iter().map(|t| t.len()).max().unwrap_or(0);

    // Indent the optional "use when:" line to align under the description
    // column: 2 leading spaces + name column + 2 spaces + tag column + 2
    // spaces + "— " (2 chars).
    let hint_indent = " ".repeat(max_name + max_tag + 8);

    let mut rows = Vec::new();
    for (w, tag) in installed.workflows.iter().zip(&tags) {
        rows.push(format!(
            "  {:<name_w$}  {:<tag_w$}  — {}",
            w.name,
            tag,
            w.def.description,
            name_w = max_name,
            tag_w = max_tag,
        ));
        if let Some(hint) = &w.def.when_to_use {
            rows.push(format!("{hint_indent}use when: {hint}"));
        }
    }

    let warnings = installed
        .errors
        .into_iter()
        .map(|(name, err)| format!("warning: skipping workflow '{name}' ({err})"))
        .collect();

    Ok(ListOutput { rows, warnings })
}

/// Look up a single workflow's definition for display (used by
/// `pm feat info` to show the workflow row alongside its description).
/// Returns `None` if the workflow isn't installed in either tier; an `Err`
/// only on a genuine I/O or parse failure for an installed workflow.
pub fn get(project_root: &Path, name: &str) -> Result<Option<WorkflowDef>> {
    match WorkflowDef::load(project_root, name) {
        Ok(def) => Ok(Some(def)),
        Err(PmError::WorkflowNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;

    fn setup_project_root() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(paths::features_dir(&root)).unwrap();
        (dir, root)
    }

    /// Rows for the project tier alone, against an empty global tier.
    fn project_rows(root: &Path) -> ListOutput {
        let empty = tempdir().unwrap();
        list_rows_in(Some(root), empty.path()).unwrap()
    }

    fn write_workflow(project_root: &Path, name: &str, body: &str, md: &str) {
        let dir = paths::workflows_dir(project_root).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
        std::fs::write(dir.join("workflow.md"), md).unwrap();
    }

    fn write_feature_state(features_dir: &Path, name: &str, workflow: Option<&str>) {
        let now = Utc::now();
        let state = FeatureState {
            status: crate::state::feature::FeatureStatus::Wip,
            branch: name.to_string(),
            worktree: name.to_string(),
            base: "main".to_string(),
            pr: String::new(),
            context: String::new(),
            workflow: workflow.map(|s| s.to_string()),
            created: now,
            last_active: now,
        };
        state.save(features_dir, name).unwrap();
    }

    #[test]
    fn show_returns_md_content_when_feature_has_workflow() {
        let (_dir, root) = setup_project_root();
        write_workflow(&root, "demo", "description = \"d\"\n", "# demo\nbody");
        write_feature_state(&paths::features_dir(&root), "feat", Some("demo"));

        let body = show(&root, "feat").unwrap().unwrap();
        // The workflow.md content comes first, with the summary guidance
        // appended after a blank-line separator.
        assert!(body.starts_with("# demo\nbody\n\n"));
        assert!(body.contains("## summary.md"));
    }

    #[test]
    fn show_appends_summary_guidance_once() {
        let (_dir, root) = setup_project_root();
        // workflow.md with no trailing newline — guidance must still be
        // separated by exactly one blank line and appended a single time.
        write_workflow(&root, "demo", "description = \"d\"\n", "# demo");
        write_feature_state(&paths::features_dir(&root), "feat", Some("demo"));

        let body = show(&root, "feat").unwrap().unwrap();
        assert!(body.starts_with("# demo\n\n## summary.md\n"));
        assert_eq!(body.matches("## summary.md").count(), 1);
        // so an owner with no file yet knows to write one
        assert!(body.contains("create `summary.md`"));
        assert!(body.contains("high signal-to-noise"));
    }

    #[test]
    fn show_separates_guidance_when_md_ends_in_newline() {
        let (_dir, root) = setup_project_root();
        // The realistic production case: bundled workflow.md files end in
        // a trailing newline, so the separator must be exactly one blank
        // line (not two).
        write_workflow(&root, "demo", "description = \"d\"\n", "# demo\n");
        write_feature_state(&paths::features_dir(&root), "feat", Some("demo"));

        let body = show(&root, "feat").unwrap().unwrap();
        assert!(body.starts_with("# demo\n\n## summary.md\n"));
        assert_eq!(body.matches("## summary.md").count(), 1);
    }

    #[test]
    fn show_returns_none_when_feature_has_no_workflow() {
        let (_dir, root) = setup_project_root();
        write_feature_state(&paths::features_dir(&root), "feat", None);

        assert!(show(&root, "feat").unwrap().is_none());
    }

    #[test]
    fn show_returns_none_in_main_scope() {
        let (_dir, root) = setup_project_root();
        assert!(show(&root, "main").unwrap().is_none());
    }

    #[test]
    fn show_errors_when_workflow_md_missing() {
        let (_dir, root) = setup_project_root();
        // Feature points to a workflow, but the workflow.md isn't there.
        write_feature_state(&paths::features_dir(&root), "feat", Some("ghost"));

        let err = show(&root, "feat").unwrap_err();
        assert!(matches!(err, PmError::WorkflowNotFound(_)));
    }

    #[test]
    fn list_rows_returns_sorted_rows_with_descriptions() {
        let (_dir, root) = setup_project_root();
        write_workflow(&root, "beta", "description = \"second\"\n", "# beta");
        write_workflow(&root, "alpha", "description = \"first\"\n", "# alpha");

        let out = project_rows(&root);
        assert_eq!(out.rows.len(), 2);
        assert!(out.rows[0].contains("alpha"));
        assert!(out.rows[0].contains("first"));
        assert!(out.rows[1].contains("beta"));
        assert!(out.rows[1].contains("second"));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn list_rows_renders_when_to_use_hint() {
        let (_dir, root) = setup_project_root();
        write_workflow(
            &root,
            "demo",
            "description = \"d\"\nwhen_to_use = \"pick me for X\"\n",
            "# demo",
        );
        let out = project_rows(&root);
        // Description row plus the hint row.
        assert_eq!(out.rows.len(), 2);
        assert!(out.rows[0].contains("demo"));
        assert!(out.rows[1].contains("use when: pick me for X"));
    }

    #[test]
    fn list_rows_omits_hint_row_when_absent() {
        let (_dir, root) = setup_project_root();
        write_workflow(&root, "demo", "description = \"d\"\n", "# demo");
        let out = project_rows(&root);
        // No hint → single row, no `use when:` line.
        assert_eq!(out.rows.len(), 1);
        assert!(!out.rows.iter().any(|r| r.contains("use when:")));
    }

    #[test]
    fn list_rows_tags_tier_and_origin_with_project_shadowing_global() {
        let (_dir, root) = setup_project_root();
        crate::commands::skills::install_global().unwrap();
        write_workflow(&root, "solo", "description = \"my solo\"\n", "# mine");
        write_workflow(&root, "extra", "description = \"extra\"\n", "# extra");

        let out = list_rows(Some(&root)).unwrap();
        let find = |name: &str| {
            out.rows
                .iter()
                .find(|r| r.trim_start().starts_with(&format!("{name} ")))
                .unwrap_or_else(|| panic!("no row for {name}: {:?}", out.rows))
                .clone()
        };
        assert!(find("solo").contains("[project, custom]") && find("solo").contains("my solo"));
        assert!(find("extra").contains("[project, custom]"));
        assert!(find("pr-review").contains("[global, bundled]"));
        assert_eq!(out.rows.iter().filter(|r| r.contains("solo ")).count(), 1);

        // Outside a project only the global tier shows.
        let global = list_rows(None).unwrap();
        assert!(global.rows.iter().all(|r| !r.contains("[project")));
        assert!(
            global
                .rows
                .iter()
                .any(|r| r.contains("solo") && r.contains("[global, bundled]"))
        );
    }

    #[test]
    fn list_rows_surfaces_broken_workflow_as_warning() {
        let (_dir, root) = setup_project_root();
        write_workflow(&root, "ok", "description = \"good\"\n", "# ok");
        // A directory with a broken config.toml — produces a warning,
        // not a hard error.
        let bad_dir = paths::workflows_dir(&root).join("bad");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("config.toml"), "not = valid {{{").unwrap();

        let out = project_rows(&root);
        assert_eq!(out.rows.len(), 1);
        assert!(out.rows[0].contains("ok"));
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("bad"));
        assert!(out.warnings[0].starts_with("warning: skipping workflow"));
    }

    #[test]
    fn get_returns_def_when_workflow_exists() {
        let (_dir, root) = setup_project_root();
        write_workflow(&root, "demo", "description = \"d\"\n", "# demo");
        let def = get(&root, "demo").unwrap().unwrap();
        assert_eq!(def.description, "d");
    }

    #[test]
    fn get_returns_none_for_missing_workflow() {
        let (_dir, root) = setup_project_root();
        assert!(get(&root, "missing").unwrap().is_none());
    }
}
