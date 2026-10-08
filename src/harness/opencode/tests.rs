use serde_json::{Value, json};

use super::api::detached;
use super::test_support::fake_opencode;
use super::*;
use crate::state::paths;
use crate::testing::{fake_opencode_argv, fake_opencode_calls};

fn cfg() -> OpenCodeConfig {
    OpenCodeConfig::default()
}

fn pre(session: &str) -> PreLaunch {
    PreLaunch {
        session_id: Some(session.to_string()),
        env: vec![
            (SESSION_ENV.to_string(), session.to_string()),
            (PROMPT_ENV.to_string(), "/x/base line.md".to_string()),
        ],
        env_remove: vec![CONFIG_CONTENT_ENV.to_string()],
        notes: Vec::new(),
    }
}

fn with_model() -> SpawnSpec<'static> {
    SpawnSpec {
        model: Some("local/qwen"),
        ..Default::default()
    }
}

fn ctx<'a>(dir: &'a Path, agent: &'a str) -> LaunchContext<'a> {
    LaunchContext {
        project_root: dir,
        feature: "login",
        worktree: dir,
        agent,
    }
}

/// `definition` as `pm upgrade` projects it into `worktree`.
fn project_definition(worktree: &Path, definition: &str) {
    let agents = worktree.join(CONFIG_DIR).join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join(format!("{definition}.md")), "# stub").unwrap();
}

fn env_of<'a>(pre: &'a PreLaunch, key: &str) -> Option<&'a str> {
    pre.env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn written_config(pre: &PreLaunch) -> Value {
    let path = env_of(pre, CONFIG_ENV).expect("every spawn names a config file");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The JSON body of an `api … --data <body>` call.
fn body_of(call: &[String]) -> Value {
    let at = call.iter().position(|a| a == "--data").expect("--data");
    serde_json::from_str(&call[at + 1]).unwrap()
}

#[test]
fn build_cmd_opens_the_prepared_session_standalone_and_auto() {
    let cmd = build_cmd(
        &SpawnSpec {
            definition: Some("reviewer"),
            prompt: Some("Stand by."),
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg(),
        &pre("ses_1"),
    );
    // The definition, model and prompt never reach the command
    // line: the session already carries the agent and the model, and the
    // plugin arms without a first turn.
    assert_eq!(
        cmd,
        "'env' '-u' 'OPENCODE_CONFIG_CONTENT' 'PM_OPENCODE_SESSION=ses_1' \
         'PM_APPEND_PROMPT_FILE=/x/base line.md' 'opencode' '--standalone' '--auto' \
         '--session' 'ses_1'"
    );
}

#[test]
fn build_cmd_quotes_a_session_id_the_shell_would_act_on() {
    let cmd = build_cmd(
        &SpawnSpec::default(),
        &cfg(),
        &PreLaunch {
            session_id: Some("ses_1; touch 'x' $(id)".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(
        cmd,
        "'env' 'opencode' '--standalone' '--auto' '--session' 'ses_1; touch '\\''x'\\'' $(id)'"
    );
}

#[test]
fn build_cmd_drops_auto_only_when_configured_off() {
    let off = OpenCodeConfig {
        auto: Some(false),
        binary: Some("/opt/open code/bin/opencode".into()),
        ..Default::default()
    };
    assert_eq!(
        build_cmd(&SpawnSpec::default(), &off, &PreLaunch::default()),
        "'env' '/opt/open code/bin/opencode' '--standalone'"
    );
    let on = OpenCodeConfig {
        auto: Some(true),
        ..Default::default()
    };
    assert_eq!(
        build_cmd(&SpawnSpec::default(), &on, &PreLaunch::default()),
        "'env' 'opencode' '--standalone' '--auto'"
    );
}

#[test]
fn every_command_line_is_standalone() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);

    for spec in [
        SpawnSpec {
            model: Some("local/qwen"),
            ..Default::default()
        },
        SpawnSpec {
            resume_session: Some("ses_old"),
            model: Some("local/qwen"),
            ..Default::default()
        },
        SpawnSpec {
            resume_session: Some("ses_old"),
            fork_session: true,
            model: Some("local/qwen"),
            ..Default::default()
        },
    ] {
        let before = fake_opencode_calls(dir.path()).len();
        let pre = pre_launch(&ctx(dir.path(), "reviewer"), &spec, &cfg).unwrap();
        let calls = fake_opencode_calls(dir.path());
        assert!(calls.len() > before, "{spec:?} made no call");
        for call in &calls[before..] {
            assert_eq!(&call[..2], ["api", "--standalone"], "{spec:?}");
        }
        let cmd = build_cmd(&spec, &cfg, &pre);
        assert!(
            cmd.contains("/opencode' '--standalone' '--auto' '--session' "),
            "{spec:?}: {cmd}"
        );
    }
}

#[test]
fn pre_launch_creates_a_session_for_the_definition_in_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    let worktree = project.join("login");
    project_definition(&worktree, "rev");
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new","agent":"rev"}}"#, 0);

    let pre = pre_launch(
        &LaunchContext {
            project_root: &project,
            feature: "login",
            worktree: &worktree,
            agent: "frontend-rev",
        },
        &SpawnSpec {
            definition: Some("rev"),
            append_prompt_file: Some("/x/baseline.md"),
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap();

    assert_eq!(pre.session_id.as_deref(), Some("ses_new"));
    assert_eq!(env_of(&pre, SESSION_ENV), Some("ses_new"));
    assert_eq!(env_of(&pre, PROMPT_ENV), Some("/x/baseline.md"));
    assert_eq!(
        env_of(&pre, TRIP_ENV),
        Some(
            trip_file(&project, "login", "frontend-rev")
                .unwrap()
                .to_str()
                .unwrap()
        )
    );

    let calls = fake_opencode_calls(dir.path());
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        &calls[0][..4],
        ["api", "--standalone", "session.create", "--data"]
    );
    assert_eq!(
        body_of(&calls[0]),
        json!({
            "location": {"directory": worktree.canonicalize().unwrap()},
            "title": "pm:frontend-rev",
            "agent": "rev",
            "model": {"providerID": "local", "id": "qwen"},
        })
    );
}

#[test]
fn a_spawn_never_runs_on_the_config_of_whoever_spawned_it() {
    // pm often runs inside an agent's shell, which carries that agent's
    // identity and opencode config.
    let command = detached("opencode");
    let removed: Vec<&str> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .filter_map(|(key, _)| key.to_str())
        .collect();
    for key in [
        "PM_AGENT_NAME",
        paths::AGENT_WORKTREE_ENV,
        SESSION_ENV,
        PROMPT_ENV,
        TRIP_ENV,
        CONFIG_ENV,
        CONFIG_CONTENT_ENV,
    ] {
        assert!(removed.contains(&key), "{key} reaches opencode");
    }

    // The agent's own file replaces an inherited `OPENCODE_CONFIG`, and
    // the inline form is removed outright.
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    let pre = pre_launch(&ctx(dir.path(), "reviewer"), &with_model(), &cfg).unwrap();
    assert_eq!(pre.env_remove, [CONFIG_CONTENT_ENV]);
    let cmd = build_cmd(&with_model(), &cfg, &pre);
    assert!(
        cmd.starts_with("'env' '-u' 'OPENCODE_CONFIG_CONTENT' "),
        "{cmd}"
    );
    assert!(
        cmd.contains(&format!(
            "'OPENCODE_CONFIG={}'",
            env_of(&pre, CONFIG_ENV).unwrap()
        )),
        "{cmd}"
    );
}

#[test]
fn pre_launch_vanilla_session_names_no_agent() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    pre_launch(&ctx(dir.path(), "plain"), &with_model(), &cfg).unwrap();
    let body = body_of(&fake_opencode_argv(dir.path()));
    assert!(body.get("agent").is_none(), "{body}");
}

#[test]
fn an_agent_without_a_model_row_is_refused_before_any_call() {
    // Left to pick a model itself, opencode runs a hosted one.
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    for (resume_session, fork_session) in [
        (None, false),
        (Some("ses_old"), false),
        (Some("ses_old"), true),
    ] {
        let err = pre_launch(
            &ctx(dir.path(), "frontend-rev"),
            &SpawnSpec {
                definition: Some("reviewer"),
                resume_session,
                fork_session,
                ..Default::default()
            },
            &cfg,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("opencode agent 'frontend-rev' has no [agents.models] row"),
            "{err}"
        );
        assert!(
            err.ends_with("Set `[agents.models] reviewer = \"<provider>/<model>\"`"),
            "{err}"
        );
    }
    let err = pre_launch(&ctx(dir.path(), "plain"), &SpawnSpec::default(), &cfg)
        .unwrap_err()
        .to_string();
    assert!(err.contains("`[agents.models] plain = "), "{err}");
    assert!(fake_opencode_calls(dir.path()).is_empty());
}

#[test]
fn configured_providers_are_written_and_all_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    cfg.providers = [
        (
            "local".to_string(),
            "package = \"pkg\"\nsettings = { baseURL = \"http://127.0.0.1:8000/v1\" }"
                .parse()
                .unwrap(),
        ),
        (
            "second".to_string(),
            "package = \"pkg\"\nmodels = { small = {} }"
                .parse()
                .unwrap(),
        ),
    ]
    .into();
    let spec = SpawnSpec {
        model: Some("local/qwen"),
        ..Default::default()
    };
    let pre = pre_launch(&ctx(dir.path(), "reviewer"), &spec, &cfg).unwrap();
    assert_eq!(
        written_config(&pre),
        json!({
            "model": "local/qwen",
            "enabled_providers": ["local", "second"],
            "providers": {
                "local": {
                    "package": "pkg",
                    "settings": {"baseURL": "http://127.0.0.1:8000/v1"},
                    "models": {"qwen": {}},
                },
                "second": {"package": "pkg", "models": {"small": {}}},
            },
        })
    );
    assert!(pre.notes.is_empty(), "{:?}", pre.notes);
}

#[test]
fn a_stored_key_is_refused_before_any_call_and_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    cfg.providers = [(
        "local".to_string(),
        "settings = { apiKey = \"sk-live-123\" }".parse().unwrap(),
    )]
    .into();
    let context = ctx(dir.path(), "keyed");
    let err = pre_launch(&context, &with_model(), &cfg)
        .unwrap_err()
        .to_string();
    assert!(err.contains("`settings.apiKey` must name"), "{err}");
    assert!(fake_opencode_calls(dir.path()).is_empty());
    let file = spawn_file(context.project_root, context.feature, context.agent, "json");
    assert!(!file.unwrap().exists());
}

#[test]
fn a_provider_whose_key_is_unset_is_remarked_on_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    cfg.providers = [
        (
            "local".to_string(),
            "env = [\"PM_TEST_KEY_NOBODY_SETS\"]".parse().unwrap(),
        ),
        // Not the provider of this agent's row.
        (
            "hosted".to_string(),
            "env = [\"PM_TEST_OTHER_KEY_NOBODY_SETS\"]".parse().unwrap(),
        ),
    ]
    .into();
    let pre = pre_launch(&ctx(dir.path(), "reviewer"), &with_model(), &cfg).unwrap();
    assert_eq!(pre.notes.len(), 1, "{:?}", pre.notes);
    assert!(
        pre.notes[0].contains("provider 'local' takes its key from PM_TEST_KEY_NOBODY_SETS"),
        "{:?}",
        pre.notes
    );
}

#[test]
fn a_model_row_is_pinned_on_the_session_and_limits_the_agent_to_its_provider() {
    // A model that only config names is one opencode replaces with its
    // default when it cannot resolve it; a pinned one fails the turn.
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    let pre = pre_launch(
        &ctx(dir.path(), "reviewer"),
        &SpawnSpec {
            model: Some("local/mlx-community/Qwen3.8-27B-4bit#high"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap();

    assert_eq!(
        body_of(&fake_opencode_argv(dir.path()))["model"],
        json!({
            "providerID": "local",
            "id": "mlx-community/Qwen3.8-27B-4bit",
            "variant": "high",
        })
    );
    assert_eq!(
        written_config(&pre),
        json!({
            "model": "local/mlx-community/Qwen3.8-27B-4bit#high",
            "enabled_providers": ["local"],
        })
    );
}

#[test]
fn a_model_row_opencode_could_not_pin_is_refused_before_any_call() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    for row in ["opus", "/opus", "anthropic/", "anthropic/opus#", ""] {
        let err = pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec {
                model: Some(row),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.ends_with(&format!(
                "must be `<provider>/<model>[#variant]`; got: {row}"
            )),
            "{row}: {err}"
        );
    }
    assert!(fake_opencode_calls(dir.path()).is_empty());
}

#[test]
fn pre_launch_resume_pins_the_row_as_it_is_now() {
    // The session may predate the row, or carry the row's old value.
    let dir = tempfile::tempdir().unwrap();
    // What opencode prints for a call with nothing to return.
    let cfg = fake_opencode(dir.path(), "", 0);
    let pre = pre_launch(
        &ctx(dir.path(), "reviewer"),
        &SpawnSpec {
            resume_session: Some("ses_stored"),
            model: Some("anthropic/claude-opus-5"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap();
    assert_eq!(pre.session_id.as_deref(), Some("ses_stored"));
    let calls = fake_opencode_calls(dir.path());
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        &calls[0][..5],
        [
            "api",
            "--standalone",
            "session.switchModel",
            "--param",
            "sessionID=ses_stored"
        ]
    );
    assert_eq!(
        body_of(&calls[0]),
        json!({"model": {"providerID": "anthropic", "id": "claude-opus-5"}})
    );
}

#[test]
fn resuming_a_session_opencode_no_longer_has_starts_a_fresh_one_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    project_definition(dir.path(), "reviewer");
    // What opencode 2.0.18 answers for a session its store does not hold.
    let missing = "{\"_tag\":\"SessionNotFoundError\",\"sessionID\":\"ses_gone\",\
                   \"message\":\"Session not found: ses_gone\"}\nHTTP 404 Not Found";
    let cfg = OpenCodeConfig {
        binary: Some(crate::testing::fake_opencode_scripted(
            dir.path(),
            &[(missing, 1), (r#"{"data":{"id":"ses_new"}}"#, 0)],
        )),
        ..Default::default()
    };

    let pre = pre_launch(
        &ctx(dir.path(), "reviewer"),
        &SpawnSpec {
            definition: Some("reviewer"),
            resume_session: Some("ses_gone"),
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap();

    assert_eq!(pre.session_id.as_deref(), Some("ses_new"));
    assert_eq!(
        pre.notes,
        ["opencode no longer has session ses_gone; previous session not resumed"]
    );
    let calls = fake_opencode_calls(dir.path());
    assert_eq!(calls[1][2], "session.create");
    assert_eq!(body_of(&calls[1])["agent"], "reviewer");
}

#[test]
fn a_resume_opencode_refuses_for_another_reason_is_reported_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(
        dir.path(),
        "{\"_tag\":\"InvalidRequestError\",\"message\":\"database is locked\"}",
        1,
    );
    let err = pre_launch(
        &ctx(dir.path(), "reviewer"),
        &SpawnSpec {
            resume_session: Some("ses_x"),
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.ends_with("opencode session.switchModel failed: database is locked"),
        "{err}"
    );
    assert_eq!(fake_opencode_calls(dir.path()).len(), 1);
}

#[test]
fn a_definition_opencode_would_not_find_is_refused_with_the_fix_for_its_worktree() {
    // opencode runs an unknown agent name on its built-in prompt.
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    let main = dir.path().join("main");
    let login = dir.path().join("login");
    let spec = SpawnSpec {
        definition: Some("def"),
        model: Some("local/qwen"),
        ..Default::default()
    };
    let refusal = |feature: &str, worktree: &Path| {
        let ctx = LaunchContext {
            project_root: dir.path(),
            feature,
            worktree,
            agent: "frontend-rev",
        };
        pre_launch(&ctx, &spec, &cfg).unwrap_err().to_string()
    };
    let refused = "definition 'def' is not projected for opencode, which would run agent \
                   'frontend-rev' without its role; ";

    // `pm upgrade` projects into main, never into a feature worktree.
    let err = refusal("main", &main);
    assert!(
        err.ends_with(&format!("{refused}run `pm upgrade`")),
        "{err}"
    );
    let err = refusal("login", &login);
    assert!(
        err.ends_with(&format!(
            "{refused}run `pm upgrade`, then `pm harness pull login`"
        )),
        "{err}"
    );
    project_definition(&main, "def");
    let err = refusal("login", &login);
    assert!(
        err.ends_with(&format!("{refused}run `pm harness pull login`")),
        "{err}"
    );
    assert!(fake_opencode_calls(dir.path()).is_empty());

    project_definition(&login, "def");
    let ctx = LaunchContext {
        project_root: dir.path(),
        feature: "login",
        worktree: &login,
        agent: "frontend-rev",
    };
    pre_launch(&ctx, &spec, &cfg).unwrap();
}

#[test]
fn a_model_its_provider_does_not_declare_is_remarked_on_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
    cfg.providers = [(
        "local".to_string(),
        "models = { \"Qwen3.8-27B-4bit\" = {} }".parse().unwrap(),
    )]
    .into();
    let spawn = |row| {
        pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec {
                model: Some(row),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap()
        .notes
    };
    assert_eq!(
        spawn("local/Qwen3.8-27B-4bi"),
        [
            "model 'Qwen3.8-27B-4bi' is not among those [harness.opencode.providers.local] \
          declares (Qwen3.8-27B-4bit); if it is a typo, every turn fails at the endpoint"
        ]
    );
    // Written into the config all the same: the endpoint may serve it.
    assert_eq!(
        written_config(
            &pre_launch(
                &ctx(dir.path(), "reviewer"),
                &SpawnSpec {
                    model: Some("local/Qwen3.8-27B-4bi"),
                    ..Default::default()
                },
                &cfg,
            )
            .unwrap()
        )["providers"]["local"]["models"]["Qwen3.8-27B-4bi"],
        json!({})
    );
    assert!(spawn("local/Qwen3.8-27B-4bit").is_empty());
}

#[test]
fn a_fork_opencode_cannot_then_find_to_pin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = OpenCodeConfig {
        binary: Some(crate::testing::fake_opencode_scripted(
            dir.path(),
            &[
                (r#"{"data":{"id":"ses_fork"}}"#, 0),
                (
                    r#"{"_tag":"SessionNotFoundError","message":"Session not found: ses_fork"}"#,
                    1,
                ),
            ],
        )),
        ..Default::default()
    };
    let err = pre_launch(
        &ctx(dir.path(), "reviewer-2"),
        &SpawnSpec {
            resume_session: Some("ses_source"),
            fork_session: true,
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.ends_with(
            "opencode forked session ses_source as ses_fork, then did not find ses_fork to \
             pin the model on"
        ),
        "{err}"
    );
}

#[test]
fn pre_launch_fork_asks_opencode_for_a_new_session_and_pins_the_row_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_fork"}}"#, 0);
    let pre = pre_launch(
        &ctx(dir.path(), "reviewer-2"),
        &SpawnSpec {
            resume_session: Some("ses_source"),
            fork_session: true,
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap();
    assert_eq!(pre.session_id.as_deref(), Some("ses_fork"));
    let calls = fake_opencode_calls(dir.path());
    assert_eq!(
        calls[0],
        [
            "api",
            "--standalone",
            "session.fork",
            "--param",
            "sessionID=ses_source",
            "--data",
            "{}"
        ]
    );
    assert_eq!(
        &calls[1][2..5],
        ["session.switchModel", "--param", "sessionID=ses_fork"]
    );
    assert_eq!(calls.len(), 2);
}

#[test]
fn pre_launch_reports_opencodes_own_error_message() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fake_opencode(
        dir.path(),
        "{\"_tag\":\"InvalidRequestError\",\"message\":\"Cannot fork empty session: ses_x\",\
         \"kind\":\"empty_session\"}\nHTTP 400 Bad Request",
        1,
    );
    let err = pre_launch(
        &ctx(dir.path(), "reviewer-2"),
        &SpawnSpec {
            resume_session: Some("ses_x"),
            fork_session: true,
            model: Some("local/qwen"),
            ..Default::default()
        },
        &cfg,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.ends_with("opencode session.fork failed: Cannot fork empty session: ses_x"),
        "{err}"
    );
}

#[test]
fn pre_launch_errors_when_the_binary_is_missing_or_answers_without_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let missing = OpenCodeConfig {
        binary: Some(dir.path().join("nope").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let err = pre_launch(&ctx(dir.path(), "reviewer"), &with_model(), &missing)
        .unwrap_err()
        .to_string();
    assert!(err.contains("could not run"), "{err}");

    for answer in [r#"{"data":{}}"#, ""] {
        let cfg = fake_opencode(dir.path(), answer, 0);
        let err = pre_launch(&ctx(dir.path(), "reviewer"), &with_model(), &cfg)
            .unwrap_err()
            .to_string();
        assert!(err.contains("returned no session id"), "{answer}: {err}");
    }
}
