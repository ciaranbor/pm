use super::*;

#[test]
fn feat_new_with_workflow_spawns_named_agent() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    feat_new(&FeatNewParams {
        context: Some("Build the login page"),
        workflow: Some("research-only"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    // The researcher window should exist (research-only's sole team agent)
    let session = tmux::session_name(&project_name, "login");
    let target = tmux::find_window(server.name(), &session, "researcher").unwrap();
    assert!(target.is_some(), "expected a 'researcher' tmux window");

    // The agent should be registered in the agent registry
    let agents_dir = paths::agents_dir(&project_path);
    let registry = crate::state::agent::AgentRegistry::load(&agents_dir, "login").unwrap();
    let entry = registry.get("researcher");
    assert!(entry.is_some(), "expected 'researcher' in agent registry");
}

#[test]
fn feat_new_context_without_workflow_defaults_to_solo() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());

    feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: None,
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    // The default workflow is recorded in state so `pm workflow show`
    // resolves routing for the spawned agent.
    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.workflow.as_deref(), Some("solo"));

    // solo's sole team member is the reserved vanilla `plain` name —
    // spawned and registered despite having no definition file.
    assert_vanilla_spawned(&project_path, &project_name, server.name(), "plain");
}

/// The vanilla agent spawned under `name`: a window, a registry entry
/// with no definition, and exactly one queued brief.
fn assert_vanilla_spawned(
    project_path: &Path,
    project_name: &str,
    server_name: Option<&str>,
    name: &str,
) {
    let session = tmux::session_name(project_name, "login");
    let target = tmux::find_window(server_name, &session, name).unwrap();
    assert!(target.is_some(), "expected a '{name}' tmux window");
    let agents_dir = paths::agents_dir(project_path);
    let registry = crate::state::agent::AgentRegistry::load(&agents_dir, "login").unwrap();
    let entry = registry.get(name).expect("vanilla agent in registry");
    assert!(
        entry.agent_definition.is_none(),
        "vanilla agent must not store a definition"
    );
    let messages_dir = paths::messages_dir(project_path);
    let summaries = crate::messages::list(&messages_dir, "login", name, None).unwrap();
    assert_eq!(summaries.len(), 1);
}

#[test]
fn feat_new_old_solo_naming_claude_fails_until_upgrade_removes_it() {
    // `claude` is no longer a vanilla alias: an un-upgraded project's
    // `.pm/workflows/solo` still names it and shadows the global bundled
    // one, so validation fails until the migration deletes it.
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
    let cfg = paths::workflows_dir(&project_path).join("solo");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "description = \"old solo\"\nagents = [\"claude\"]\nbrief_agents = [\"claude\"]\n",
    )
    .unwrap();
    // Pre-migration: the stale copy is pm's, not a user override.
    std::fs::remove_file(paths::migrations_dir(&project_path).join("global-assets")).unwrap();

    let err = feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: None,
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap_err();
    assert!(
        matches!(err, PmError::WorkflowAgentMissing { .. }),
        "{err:?}"
    );

    crate::commands::upgrade::upgrade_project(&project_path).unwrap();
    assert!(
        !cfg.exists(),
        "migration must remove the stale bundled solo"
    );
    feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: None,
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();
    assert_vanilla_spawned(&project_path, &project_name, server.name(), "plain");
}

#[test]
fn feat_new_resolves_a_global_custom_workflow() {
    // A workflow installed only in the global tier, naming a team whose
    // definitions live only in the global store, spawns like any other.
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
    // The shared test home's global tier is every test's: remove the
    // workflow however this test ends.
    struct RemoveOnDrop(std::path::PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let guard = RemoveOnDrop(
        paths::global_workflows_dir()
            .unwrap()
            .join("global-custom-flow"),
    );
    let wf = &guard.0;
    std::fs::create_dir_all(wf).unwrap();
    std::fs::write(
        wf.join("config.toml"),
        "description = \"global custom\"\nagents = [\"implementer\"]\n\
         brief_agents = [\"implementer\"]\n",
    )
    .unwrap();
    std::fs::write(wf.join("workflow.md"), "# global custom\nrouting prose\n").unwrap();

    feat_new(&FeatNewParams {
        workflow: Some("global-custom-flow"),
        context: Some("do X"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    let session = tmux::session_name(&project_name, "login");
    assert!(
        tmux::find_window(server.name(), &session, "implementer")
            .unwrap()
            .is_some()
    );
    let body = crate::commands::workflow::show(&project_path, "login")
        .unwrap()
        .unwrap();
    assert!(body.contains("routing prose"), "{body}");
}

#[test]
fn feat_new_workflow_without_context_spawns_idle_team() {
    // `pm feat new my-feat --workflow X` (no --context) now stands up
    // the full idle team. It records the workflow in state, spawns
    // every team agent, but queues no messages — the user drives them.
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    feat_new(&FeatNewParams {
        workflow: Some("implement-and-review"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.workflow.as_deref(), Some("implement-and-review"));

    // Whole team spawned even without --context.
    let agents_dir = paths::agents_dir(&project_path);
    let registry = crate::state::agent::AgentRegistry::load(&agents_dir, "login").unwrap();
    assert!(
        registry.get("implementer").is_some(),
        "team spawned without --context"
    );
    assert!(
        registry.get("reviewer").is_some(),
        "team spawned without --context"
    );

    // But no brief queued to anyone.
    let messages_dir = paths::messages_dir(&project_path);
    assert!(
        crate::messages::list(&messages_dir, "login", "implementer", None)
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::messages::list(&messages_dir, "login", "reviewer", None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn feat_new_refuses_a_team_whose_harnesses_cannot_run_it_and_creates_nothing() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
    let pm_dir = paths::pm_dir(&project_path);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    // No model row for the opencode member; the other names no harness
    // pm can spawn.
    for (definition, harness) in [("implementer", "opencode"), ("reviewer", "aider")] {
        config
            .agents
            .harness
            .insert(definition.to_string(), harness.to_string());
    }
    config.harness.opencode.binary = Some(crate::testing::fake_opencode(
        dir.path(),
        "opencode v2.0.23",
        0,
    ));
    config.save(&pm_dir).unwrap();

    let err = feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: Some("implement-and-review"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("2 of 2 team member(s) cannot run on their harness"),
        "{err}"
    );
    assert!(
        err.contains("\n  implementer (opencode): no [agents.models] row"),
        "{err}"
    );
    assert!(
        err.contains("\n  reviewer: harness 'aider' is not supported yet"),
        "{err}"
    );

    assert!(!FeatureState::exists(
        &paths::features_dir(&project_path),
        "login"
    ));
    assert!(!project_path.join("login").exists());
    assert!(!git::branch_exists(&paths::main_worktree(&project_path), "login").unwrap());
    assert!(
        !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
    );
    assert!(
        crate::messages::list(
            &paths::messages_dir(&project_path),
            "login",
            "implementer",
            None
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn feat_new_nonexistent_workflow_errors() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    let result = feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: Some("does-not-exist"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    });
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), PmError::WorkflowNotFound(_)));

    // No partial state should remain on disk.
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn feat_new_workflow_with_empty_brief_agents_and_context_errors() {
    // A workflow with no brief_agents has nobody to deliver --context
    // to. Treat it the same as `--context` without `--workflow`: hard
    // error before any side effects.
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    let workflows = paths::workflows_dir(&project_path).join("empty");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("config.toml"), "description = \"x\"\n").unwrap();
    std::fs::write(workflows.join("workflow.md"), "# empty\n").unwrap();

    let result = feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: Some("empty"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, PmError::SafetyCheck(_)));
    assert!(
        err.to_string().contains("empty `brief_agents`"),
        "expected empty brief_agents error, got: {err}",
    );

    // No partial state should remain.
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn feat_new_workflow_with_empty_brief_agents_no_context_succeeds() {
    // Without --context, an empty brief_agents is fine — pm just
    // records the workflow and spawns nothing (empty team).
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    let workflows = paths::workflows_dir(&project_path).join("empty");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("config.toml"), "description = \"x\"\n").unwrap();
    std::fs::write(workflows.join("workflow.md"), "# empty\n").unwrap();

    feat_new(&FeatNewParams {
        workflow: Some("empty"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    })
    .unwrap();

    let features_dir = paths::features_dir(&project_path);
    let state = FeatureState::load(&features_dir, "login").unwrap();
    assert_eq!(state.workflow.as_deref(), Some("empty"));
}

#[test]
fn feat_new_workflow_with_unknown_team_agent_errors() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, _) = server.setup_project(dir.path());

    // Inject a workflow whose team references a missing agent.
    let workflows = paths::workflows_dir(&project_path).join("broken");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("config.toml"),
        "description = \"x\"\nagents = [\"ghost-impl\"]\nbrief_agents = [\"ghost-impl\"]\n",
    )
    .unwrap();
    std::fs::write(workflows.join("workflow.md"), "# broken\n").unwrap();

    let result = feat_new(&FeatNewParams {
        context: Some("do X"),
        workflow: Some("broken"),
        ..FeatNewParams::with_defaults(&project_path, &projects_dir, "login", server.name())
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, PmError::WorkflowAgentMissing { .. }),
        "expected WorkflowAgentMissing, got: {err}"
    );

    // No partial state should remain.
    let features_dir = paths::features_dir(&project_path);
    assert!(!FeatureState::exists(&features_dir, "login"));
}

#[test]
fn feat_new_full_team_spawned_brief_only_to_brief_agents() {
    let dir = tempdir().unwrap();
    let server = TestServer::new();
    let (project_path, projects_dir, project_name) = server.setup_project(dir.path());
    let messages_dir = paths::messages_dir(&project_path);

    for (feature, workflow, team, briefed) in [
        (
            "login",
            "research-implement-review",
            &["researcher", "implementer", "reviewer"][..],
            "researcher",
        ),
        (
            "signup",
            "implement-qa-review",
            &["implementer", "qa", "reviewer"][..],
            "implementer",
        ),
        (
            "logout",
            "research-implement-qa-review",
            &["researcher", "implementer", "qa", "reviewer"][..],
            "researcher",
        ),
    ] {
        feat_new(&FeatNewParams {
            context: Some("Research the auth flow"),
            workflow: Some(workflow),
            ..FeatNewParams::with_defaults(&project_path, &projects_dir, feature, server.name())
        })
        .unwrap_or_else(|e| panic!("{workflow}: {e}"));

        let session = tmux::session_name(&project_name, feature);
        for agent in team {
            assert!(
                tmux::find_window(server.name(), &session, agent)
                    .unwrap()
                    .is_some(),
                "{workflow}: expected '{agent}' window"
            );
            let queued = crate::messages::list(&messages_dir, feature, agent, None)
                .unwrap()
                .len();
            assert_eq!(
                queued,
                usize::from(*agent == briefed),
                "{workflow}: brief count for '{agent}'"
            );
        }
    }
}
