use super::*;
use crate::state::project::{ProjectConfig, ProjectInfo};
use tempfile::{TempDir, tempdir};

fn setup(global: &str) -> (TempDir, Files) {
    let dir = tempdir().unwrap();
    let root = dir.path().join("proj");
    ProjectConfig {
        project: ProjectInfo {
            name: "proj".to_string(),
            max_features: None,
            fresh_base: None,
        },
        agents: Default::default(),
        harness: Default::default(),
    }
    .save(&paths::pm_dir(&root))
    .unwrap();
    let global_dir = dir.path().join("config");
    std::fs::create_dir_all(&global_dir).unwrap();
    std::fs::write(global_dir.join(CONFIG_FILE), global).unwrap();
    let files = Files {
        project_root: Some(root),
        global_dir,
    };
    (dir, files)
}

fn read(files: &Files, tier: Tier) -> String {
    std::fs::read_to_string(files.path(tier).unwrap()).unwrap()
}

fn err(result: Result<impl std::fmt::Debug>) -> String {
    match result {
        Err(PmError::Config(message)) => message,
        other => panic!("expected a config error, got {other:?}"),
    }
}

#[test]
fn set_keeps_the_files_comments_and_get_reads_project_over_global() {
    let (_dir, files) = setup("# mine\n[serve]\nport = 7000 # kept\n");

    set(&files, Tier::Global, "project.max_features", "5").unwrap();
    let text = read(&files, Tier::Global);
    assert!(
        text.starts_with("# mine\n[serve]\nport = 7000 # kept\n"),
        "{text}"
    );
    assert_eq!(
        GlobalConfig::load(&files.global_dir)
            .unwrap()
            .project
            .max_features,
        Some(5)
    );

    let got = get(&files, "project.max_features", false).unwrap();
    assert_eq!(got.to_string(), "project.max_features = 5  (global)");

    set(&files, Tier::Project, "project.max_features", "2").unwrap();
    let got = get(&files, "project.max_features", false).unwrap();
    assert_eq!(
        got.to_string(),
        "project.max_features = 2  (project 'proj')"
    );
    let got = get(&files, "project.max_features", true).unwrap();
    assert_eq!(got.to_string(), "project.max_features = 5  (global)");
}

#[test]
fn refused_writes_leave_the_file_untouched() {
    let (_dir, files) = setup("[serve]\nport = 7000\n");
    let before = (read(&files, Tier::Project), read(&files, Tier::Global));

    let unknown = err(set(&files, Tier::Global, "serve.prot", "1"));
    assert!(unknown.contains("serve.port (global only)"), "{unknown}");
    assert!(unknown.contains("serve.push_hosts"), "{unknown}");
    err(set(&files, Tier::Global, "serve.port", "70000"));
    err(set(&files, Tier::Global, "upgrade.restart_agents", "maybe"));
    err(set(&files, Tier::Project, "serve.port", "7001"));
    err(set(&files, Tier::Project, "project.name", "other"));
    err(set(&files, Tier::Project, "harness.codex", "x"));
    assert!(matches!(
        set(&files, Tier::Project, "agents.harness.reviewer", "aider"),
        Err(PmError::HarnessUnsupported { .. })
    ));
    assert!(matches!(
        set(&files, Tier::Project, "agents.models.a.b", "opus"),
        Err(PmError::Config(_))
    ));

    assert_eq!(
        (read(&files, Tier::Project), read(&files, Tier::Global)),
        before
    );
}

#[test]
fn an_agent_row_reads_as_a_spawn_resolves_it() {
    let (_dir, files) = setup("[agents.models]\n\"*\" = \"sonnet\"\n");

    let got = get(&files, "agents.models.reviewer", false).unwrap();
    assert_eq!(
        got.to_string(),
        "agents.models.reviewer = \"sonnet\"  (global, row '*')"
    );

    // The global `*` row is bound to claude-code, so an agent moved onto
    // opencode doesn't get it, and has no model to spawn with.
    let out = set(&files, Tier::Project, "agents.harness.reviewer", "opencode").unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("warning: agent 'reviewer': runs on opencode")),
        "{out:?}"
    );
    let got = get(&files, "agents.models.reviewer", false).unwrap();
    assert_eq!(got.value, None, "{got}");
    assert!(
        got.source
            .as_deref()
            .is_some_and(|s| s.contains("row for '*'") && s.contains("not applied")),
        "{got}"
    );
    // The global tier alone still resolves through its `*` row.
    let got = get(&files, "agents.models.reviewer", true).unwrap();
    assert_eq!(
        got.to_string(),
        "agents.models.reviewer = \"sonnet\"  (global, row '*')"
    );

    set(
        &files,
        Tier::Project,
        "agents.models.reviewer",
        "local/qwen",
    )
    .unwrap();
    let got = get(&files, "agents.models.reviewer", false).unwrap();
    assert_eq!(
        got.to_string(),
        "agents.models.reviewer = \"local/qwen\"  (project 'proj')"
    );

    // Removing the row leaves the opencode agent without a model again.
    let out = unset(&files, Tier::Project, "agents.models.reviewer").unwrap();
    assert!(
        out.iter()
            .any(|l| l.starts_with("warning: agent 'reviewer': runs on opencode")),
        "{out:?}"
    );
}

#[test]
fn set_adds_a_key_to_a_table_written_inline() {
    let (_dir, files) = setup("[harness.opencode.providers]\nlocal = { package = \"x\" } # mine\n");
    set(
        &files,
        Tier::Global,
        "harness.opencode.providers.local.env",
        "[\"K\"]",
    )
    .unwrap();
    set(
        &files,
        Tier::Global,
        "harness.opencode.providers.local.options.baseURL",
        "http://h",
    )
    .unwrap();

    let text = read(&files, Tier::Global);
    assert!(text.contains("# mine"), "{text}");
    let local = &GlobalConfig::load(&files.global_dir)
        .unwrap()
        .harness
        .opencode
        .providers["local"];
    assert_eq!(local["package"].as_str(), Some("x"));
    assert_eq!(local["env"][0].as_str(), Some("K"));
    assert_eq!(local["options"]["baseURL"].as_str(), Some("http://h"));
}

#[test]
fn an_empty_project_value_masks_the_global_one() {
    let (_dir, files) = setup("[harness.codex]\nsandbox = \"workspace-write\"\n");
    set(&files, Tier::Project, "harness.codex.sandbox", "").unwrap();

    let got = get(&files, "harness.codex.sandbox", false).unwrap();
    assert_eq!(got.value, None, "{got}");
    assert_eq!(got.source.as_deref(), Some("project 'proj' sets it empty"));
}

#[test]
fn unset_removes_the_key_and_the_tables_it_leaves_empty() {
    let (_dir, files) = setup("[serve]\nport = 7000\n");
    set(&files, Tier::Global, "harness.codex.writable_roots", "a, b").unwrap();
    set(
        &files,
        Tier::Global,
        "harness.opencode.providers.local",
        "{ package = \"x\" }",
    )
    .unwrap();
    let global = GlobalConfig::load(&files.global_dir).unwrap();
    assert_eq!(
        global.harness.codex.writable_roots,
        Some(vec!["a".to_string(), "b".to_string()])
    );
    assert_eq!(
        global.harness.opencode.providers["local"]["package"].as_str(),
        Some("x")
    );

    unset(&files, Tier::Global, "harness.codex.writable_roots").unwrap();
    unset(
        &files,
        Tier::Global,
        "harness.opencode.providers.local.package",
    )
    .unwrap();
    let text = read(&files, Tier::Global);
    assert!(!text.contains("[harness.codex]"), "{text}");
    // An emptied provider entry stays: it masks the global one of its name.
    assert_eq!(
        GlobalConfig::load(&files.global_dir)
            .unwrap()
            .harness
            .opencode
            .providers
            .get("local"),
        Some(&toml::Table::new())
    );
    assert_eq!(
        unset(&files, Tier::Global, "serve.push_hosts").unwrap(),
        ["global: serve.push_hosts was not set"]
    );
}

#[test]
fn list_shows_the_settings_of_both_files_resolved() {
    let (_dir, files) = setup("[serve]\nport = 7000\n\n[agents.models]\nreviewer = \"opus\"\n");
    set(&files, Tier::Project, "agents.models.reviewer", "sonnet").unwrap();

    let lines = list(&files, false, Some("agents")).unwrap();
    assert_eq!(
        lines[2..],
        ["agents.models.reviewer = \"sonnet\"  (project 'proj')"]
    );
    let lines = list(&files, true, None).unwrap();
    assert_eq!(
        lines[1..],
        [
            "agents.models.reviewer = \"opus\"  (global)",
            "serve.port = 7000  (global)",
        ]
    );
}
