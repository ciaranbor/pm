use super::*;
use crate::commands::agent_spawn::test_support::record_session;
use crate::state::agent::AgentRegistry;
use crate::state::project::ProjectConfig;
use crate::tmux;

fn body(reply: &str) -> serde_json::Value {
    serde_json::from_str(reply).unwrap()
}

fn registry(f: &Fixture, scope: &str) -> AgentRegistry {
    AgentRegistry::load(&paths::agents_dir(&f.project), scope).unwrap()
}

fn inbox(f: &Fixture, scope: &str, agent: &str) -> Vec<String> {
    crate::messages::list(&paths::messages_dir(&f.project), scope, agent, None)
        .unwrap()
        .into_iter()
        .map(|m| m.first_line)
        .collect()
}

fn agent_path(f: &Fixture, scope: &str, agent: &str, action: &str) -> String {
    format!("/v1/agents/{}/{scope}/{agent}/{action}", f.project_name)
}

#[test]
fn definitions_list_plain_first_with_what_a_spawn_would_launch() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    let ours = paths::main_worktree(&f.project).join(".agents/agents");
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::write(
        ours.join("reviewer.md"),
        "---\nname: reviewer\ndescription: our reviewer\n---\n",
    )
    .unwrap();
    let pm_dir = paths::pm_dir(&f.project);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    config
        .agents
        .models
        .insert("reviewer".into(), "opus".into());
    config
        .agents
        .harness
        .insert("broken".into(), "no-such-harness".into());
    config.save(&pm_dir).unwrap();
    std::fs::write(ours.join("broken.md"), "# broken").unwrap();

    let path = format!("/v1/projects/{}/definitions", f.project_name);
    let (status, reply) = call(&f.config, "GET", &path, &phone, "");
    assert_eq!(status, 200, "{reply}");
    let listed = body(&reply)["definitions"].as_array().unwrap().clone();
    let named = |name: &str| {
        listed
            .iter()
            .find(|d| d["name"] == name)
            .unwrap_or_else(|| panic!("{name} not in {listed:#?}"))
            .clone()
    };
    assert_eq!(
        listed[0],
        serde_json::json!({
            "name": "plain",
            "description": "no definition: a vanilla session",
            "harness": "claude-code",
            "model": null,
        })
    );
    assert_eq!(
        named("reviewer"),
        serde_json::json!({
            "name": "reviewer",
            "description": "our reviewer",
            "harness": "claude-code",
            "model": "opus",
        })
    );
    assert_eq!(named("implementer")["model"], serde_json::Value::Null);
    let broken = named("broken");
    assert_eq!(broken["harness"], serde_json::Value::Null);
    assert!(
        broken["error"]
            .as_str()
            .unwrap()
            .contains("no-such-harness"),
        "{broken}"
    );

    let (status, reply) = call(&f.config, "POST", &path, &phone, "");
    assert_eq!(
        (status, body(&reply)["error"].as_str()),
        (405, Some("GET is served here"))
    );
    std::fs::remove_dir_all(paths::main_worktree(&f.project)).unwrap();
    let (status, reply) = call(&f.config, "GET", &path, &phone, "");
    assert_eq!(
        (status, body(&reply)["error"].as_str()),
        (404, Some("project not on this machine"))
    );
}

#[test]
fn a_spawn_registers_the_definition_and_queues_the_context_then_only_queues() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    let spawn = agent_path(&f, "login", "second", "spawn");

    let (status, reply) = call(
        &f.config,
        "POST",
        &spawn,
        &phone,
        r#"{"definition": "reviewer", "context": "look at login"}"#,
    );
    assert_eq!(status, 200, "{reply}");
    let reply = body(&reply);
    assert_eq!(reply["outcome"], "spawned", "{reply}");
    assert!(
        reply["spawned"]
            .as_str()
            .unwrap()
            .starts_with("Spawned agent 'second'"),
        "{reply}"
    );
    let entry = registry(&f, "login").get("second").cloned().unwrap();
    assert_eq!(entry.agent_definition.as_deref(), Some("reviewer"));
    assert!(entry.active);
    assert_eq!(inbox(&f, "login", "second"), ["look at login"]);

    let (status, reply) = call(
        &f.config,
        "POST",
        &spawn,
        &phone,
        r#"{"context": "and this"}"#,
    );
    assert_eq!(status, 200, "{reply}");
    assert_eq!(body(&reply)["outcome"], "already-active", "{reply}");
    assert_eq!(inbox(&f, "login", "second"), ["look at login", "and this"]);
}

#[test]
fn a_spawn_refused_before_anything_changed_leaves_no_agent_nor_message() {
    let f = fixture();
    let phone = pair(&f.config, "phone");

    let (status, reply) = call(
        &f.config,
        "POST",
        &agent_path(&f, "login", "helper", "spawn"),
        &phone,
        r#"{"definition": "no-such-def", "context": "hello"}"#,
    );
    assert_eq!(status, 409, "{reply}");
    assert_eq!(body(&reply)["refused"], "definition", "{reply}");

    let (status, reply) = call(
        &f.config,
        "POST",
        &agent_path(&f, "login", "bad%20name", "spawn"),
        &phone,
        "",
    );
    assert_eq!(status, 400, "{reply}");
    assert!(
        body(&reply)["error"]
            .as_str()
            .unwrap()
            .contains("invalid characters"),
        "{reply}"
    );

    let session = tmux::session_name(&f.project_name, "login");
    tmux::kill_session(f.server.name(), &session).unwrap();
    let (status, reply) = call(
        &f.config,
        "POST",
        &agent_path(&f, "login", "helper", "spawn"),
        &phone,
        r#"{"definition": "plain", "context": "hello"}"#,
    );
    assert_eq!(status, 409, "{reply}");
    let reply = body(&reply);
    assert_eq!(reply["refused"], "no-session", "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .ends_with("open the project, then retry"),
        "{reply}"
    );

    assert!(registry(&f, "login").get("helper").is_none());
    assert!(inbox(&f, "login", "helper").is_empty());
    let (status, _) = call(
        &f.config,
        "POST",
        &agent_path(&f, "ghost", "helper", "spawn"),
        &phone,
        "",
    );
    assert_eq!(status, 404);
}

#[test]
fn a_stopped_agent_starts_again_and_a_deleted_one_is_gone() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    let session = tmux::session_name(&f.project_name, "login");
    f.server
        .spawn_idle_fake_agent(&f.project, &session, "login", "implementer");
    crate::messages::send(
        &paths::messages_dir(&f.project),
        "login",
        "implementer",
        "user",
        "queued",
    )
    .unwrap();

    let (status, reply) = call(
        &f.config,
        "POST",
        &agent_path(&f, "login", "implementer", "stop"),
        &phone,
        "",
    );
    assert_eq!(
        (status, body(&reply)),
        (
            200,
            serde_json::json!({"stopped": "Stopped agent 'implementer' in login"})
        )
    );
    assert!(!registry(&f, "login").get("implementer").unwrap().active);

    let (status, reply) = call(
        &f.config,
        "POST",
        &agent_path(&f, "login", "implementer", "spawn"),
        &phone,
        "",
    );
    assert_eq!(status, 200, "{reply}");
    assert_ne!(body(&reply)["outcome"], "already-active", "{reply}");
    assert!(registry(&f, "login").get("implementer").unwrap().active);

    let delete = agent_path(&f, "login", "implementer", "delete");
    let (status, reply) = call(&f.config, "POST", &delete, &phone, "");
    assert_eq!(
        (status, body(&reply)),
        (
            200,
            serde_json::json!({"deleted": "Deleted agent 'implementer' from login"})
        )
    );
    assert!(registry(&f, "login").get("implementer").is_none());
    assert!(inbox(&f, "login", "implementer").is_empty());
    let (status, reply) = call(&f.config, "POST", &delete, &phone, "");
    assert_eq!(
        (status, body(&reply)["error"].as_str()),
        (404, Some("no such agent"))
    );
}

#[test]
fn a_fork_onto_a_taken_name_is_refused_and_one_onto_a_new_name_registers() {
    let f = fixture();
    let phone = pair(&f.config, "phone");
    for name in ["source", "taken"] {
        let (status, reply) = call(
            &f.config,
            "POST",
            &agent_path(&f, "login", name, "spawn"),
            &phone,
            r#"{"definition": "reviewer"}"#,
        );
        assert_eq!(status, 200, "{reply}");
    }
    record_session(&f.project, "login", "source", "sess-source");
    let fork = agent_path(&f, "login", "source", "fork");

    let (status, reply) = call(&f.config, "POST", &fork, &phone, r#"{"name": "taken"}"#);
    assert_eq!(status, 409, "{reply}");
    assert_eq!(
        body(&reply),
        serde_json::json!({
            "error": "agent 'taken' already exists in scope 'login'",
            "refused": "fork",
        })
    );

    let (status, reply) = call(&f.config, "POST", &fork, &phone, r#"{"name": "copy"}"#);
    assert_eq!(status, 200, "{reply}");
    assert!(
        body(&reply)["forked"]
            .as_str()
            .unwrap()
            .starts_with("Forked 'source' as 'copy'"),
        "{reply}"
    );
    let copy = registry(&f, "login").get("copy").cloned().unwrap();
    assert_eq!(copy.agent_definition.as_deref(), Some("reviewer"));

    let (status, _) = call(&f.config, "POST", &fork, &phone, "{}");
    assert_eq!(status, 400);
}
