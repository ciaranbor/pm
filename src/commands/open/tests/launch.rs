use super::*;

#[test]
fn a_launch_that_exits_is_taken_off_its_own_projects_count() {
    use crate::testing::fake_claude;
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (a, a_name) = server.setup_project_with_feature(dir.path(), "login");
    let b_name = server.scope("other");
    let b = dir.path().join(&b_name);
    let projects_dir = dir.path().join("registry");
    init::init(&b, &projects_dir, None, server.name()).unwrap();
    feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
        &b,
        &projects_dir,
        "login",
        server.name(),
    ))
    .unwrap();
    server.spawn_fake_agent(&a, &tmux::session_name(&a_name, "login"), "login", "up");
    crate::state::runtime::mark_started(&a, "login", "up").unwrap();
    let quits =
        server.spawn_dead_fake_agent(&b, &tmux::session_name(&b_name, "login"), "login", "quits");
    tmux::send_line(
        server.name(),
        &quits,
        &format!("{} 0.3", fake_claude().display()),
    )
    .unwrap();
    let opened = |project: &Path, agent: &str| OpenResult {
        sessions_restored: 0,
        agents_respawned: 1,
        main_session: String::new(),
        project_root: project.to_path_buf(),
        launched: vec![Launch {
            project_root: project.to_path_buf(),
            scope: "login".into(),
            agent: agent.into(),
        }],
        failed_launches: Vec::new(),
        warnings: Vec::new(),
    };
    let mut results = [opened(&a, "up"), opened(&b, "quits")];

    confirm_launches(results.iter_mut(), server.name());

    assert_eq!(
        results.map(|r| (r.agents_respawned, r.failed_launches.len())),
        [(1, 0), (0, 1)]
    );
}
