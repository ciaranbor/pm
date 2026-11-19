use super::*;
use crate::state::feature::FeatureState;
use crate::testing::feature_state;
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
    let global = workflow::GlobalTier::under(empty.path());
    list_rows_in(Some(root), &global, None, &Disabled::default()).unwrap()
}

fn write_workflow(project_root: &Path, name: &str, body: &str, md: &str) {
    let dir = paths::workflows_dir(project_root).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), body).unwrap();
    std::fs::write(dir.join("workflow.md"), md).unwrap();
}

fn write_feature_state(features_dir: &Path, name: &str, workflow: Option<&str>) {
    let state = FeatureState {
        base: "main".to_string(),
        workflow: workflow.map(|s| s.to_string()),
        ..feature_state(name)
    };
    state.save(features_dir, name).unwrap();
}

#[test]
fn list_marks_a_workflow_whose_disabled_member_has_no_custom() {
    let (_dir, root) = setup_project_root();
    let empty = tempdir().unwrap();
    let global = workflow::GlobalTier::under(empty.path());
    let disabled = Disabled::from_config(toml::from_str("agents = [\"qa\"]").unwrap());
    write_workflow(
        &root,
        "demo",
        "description = \"d\"\nagents = [\"qa\"]\n",
        "",
    );
    let rows = |d: &Disabled| list_rows_in(Some(&root), &global, None, d).unwrap().rows;

    assert!(
        rows(&disabled)[0].ends_with("— d (unavailable: agent 'qa' disabled)"),
        "{:?}",
        rows(&disabled)
    );
    assert!(rows(&Disabled::default())[0].ends_with("— d"));

    let agents = paths::main_worktree(&root).join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("qa.md"), "# my qa").unwrap();
    assert!(rows(&disabled)[0].ends_with("— d"));
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
    assert!(body.contains("## Status and summary"));
}

#[test]
fn show_appends_summary_guidance_once() {
    let (_dir, root) = setup_project_root();
    // workflow.md with no trailing newline — guidance must still be
    // separated by exactly one blank line and appended a single time.
    write_workflow(&root, "demo", "description = \"d\"\n", "# demo");
    write_feature_state(&paths::features_dir(&root), "feat", Some("demo"));

    let body = show(&root, "feat").unwrap().unwrap();
    assert!(body.starts_with("# demo\n\n## Status and summary\n"));
    assert_eq!(body.matches("## Status and summary").count(), 1);
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
    assert!(body.starts_with("# demo\n\n## Status and summary\n"));
    assert_eq!(body.matches("## Status and summary").count(), 1);
}

#[test]
fn show_appends_shipping_guidance_only_in_a_shipping_project() {
    let (_dir, root) = setup_project_root();
    write_workflow(&root, "demo", "description = \"d\"\n", "# demo\n");
    write_feature_state(&paths::features_dir(&root), "feat", Some("demo"));
    let mut config = crate::testing::project_config("proj");
    let pm_dir = paths::pm_dir(&root);

    config.save(&pm_dir).unwrap();
    let plain = show(&root, "feat").unwrap().unwrap();
    assert_eq!(plain, format!("# demo\n\n{SUMMARY_GUIDANCE}"));

    config.github.ship = Some(true);
    config.save(&pm_dir).unwrap();
    let shipping = show(&root, "feat").unwrap().unwrap();
    assert_eq!(
        shipping,
        format!("{plain}\n{}", shipping::SHIPPING_GUIDANCE)
    );
}

#[test]
fn show_leaves_shipping_out_of_a_review_of_someone_elses_pr() {
    let (_dir, root) = setup_project_root();
    let review = crate::commands::feat_review::REVIEW_WORKFLOW;
    write_workflow(&root, review, "description = \"d\"\n", "# review\n");
    write_feature_state(&paths::features_dir(&root), "feat", Some(review));
    let mut config = crate::testing::project_config("proj");
    config.github.ship = Some(true);
    config.save(&paths::pm_dir(&root)).unwrap();

    let body = show(&root, "feat").unwrap().unwrap();
    assert_eq!(body, format!("# review\n\n{SUMMARY_GUIDANCE}"));
}

#[test]
fn show_returns_none_when_feature_has_no_workflow() {
    let (_dir, root) = setup_project_root();
    write_feature_state(&paths::features_dir(&root), "feat", None);

    assert!(show(&root, "feat").unwrap().is_none());
}

#[test]
fn show_in_main_scope_renders_the_orchestrator_workflow() {
    let (_dir, root) = setup_project_root();
    write_workflow(
        &root,
        scope::MAIN_WORKFLOW,
        "description = \"d\"\n",
        "# mine\n",
    );

    let body = show(&root, scope::MAIN).unwrap().unwrap();
    assert!(body.starts_with("# mine\n\n## Status and summary\n"));
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
    assert!(find("solo").contains("[project, user]") && find("solo").contains("my solo"));
    assert!(find("extra").contains("[project, user]"));
    assert!(find("pr-review").contains("[global, bundled]"));
    assert!(find(scope::MAIN_WORKFLOW).ends_with("(main's scope, not for features)"));
    assert!(!find("solo").contains("main's scope"));
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
