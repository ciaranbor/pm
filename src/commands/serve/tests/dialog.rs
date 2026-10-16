use super::*;

#[test]
fn open_dialogs_are_served_and_an_answer_reaches_the_hook_of_the_one_it_names() {
    use crate::harness::Harness;
    use crate::state::runtime::{self, DialogRecord, Waiting, WaitingKind};
    let f = fixture();
    let token = pair(&f.config, "phone");
    register_conversation(&f.project, "implementer", "s1");
    let agent = format!("/v1/agents/{}/login/implementer/dialog", f.project_name);
    let get_one = || get(&f.config, &agent, Some(&token));
    let get_all = || {
        let (status, body) = get(&f.config, &format!("{agent}s"), Some(&token));
        assert_eq!(status, 200);
        let all: serde_json::Value = serde_json::from_str(&body).unwrap();
        all["dialogs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let post =
        |body: &serde_json::Value| call(&f.config, "POST", &agent, &token, &body.to_string());
    let start = chrono::Utc::now();
    let open = |command: &str, pid: u32, after: i64| {
        let payload = serde_json::json!({
            "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": {"command": command}
        });
        let (mut dialog, reply_context) = Harness::ClaudeCode.dialog(&payload).unwrap();
        dialog.since = start + chrono::Duration::seconds(after);
        let record = DialogRecord {
            dialog,
            pid: Some(pid),
            reply_context,
        };
        runtime::write_dialog(&f.project, "login", "implementer", &record).unwrap();
        record
    };
    let waiting = Waiting::now(WaitingKind::Permission, Some("Bash: cargo publish".into()));
    runtime::write_waiting(&f.project, "login", "implementer", &waiting).unwrap();

    let dead = open("cargo publish", u32::MAX / 2, 0);
    assert_eq!(get_one().0, 404, "its hook is dead");
    assert_eq!(get_all(), Vec::<String>::new());
    assert_eq!(
        post(&serde_json::json!({"id": dead.dialog.id, "choice": "allow"})),
        (
            409,
            serde_json::json!({"error": "the dialog's hook is gone", "refused": "gone"})
                .to_string()
        )
    );

    // This process stands in for both hooks.
    let older = open("cargo publish", std::process::id(), 1);
    let newer = open("git push", std::process::id(), 2);
    let (status, body) = get_one();
    assert_eq!(status, 200);
    let served: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(served["id"], older.dialog.id.as_str(), "the oldest");
    assert_eq!(served["tool"], "Bash");
    assert_eq!(served["detail"], "cargo publish");
    assert_eq!(served["choices"][1]["id"], "deny");
    assert_eq!(served.get("reply_context"), None);
    assert_eq!(served.get("pid"), None);
    assert_eq!(
        get_all(),
        [older.dialog.id.clone(), newer.dialog.id.clone()]
    );

    let unknown = post(&serde_json::json!({"id": "0123abcd", "choice": "allow"}));
    assert_eq!(unknown.0, 404, "{}", unknown.1);
    let id = newer.dialog.id.clone();
    assert_eq!(
        post(&serde_json::json!({"id": id, "choice": "maybe"})).0,
        400
    );
    assert_eq!(post(&serde_json::json!({"id": id})).0, 400);

    let project = f.project.clone();
    let taken = id.clone();
    let hook = std::thread::spawn(move || {
        for _ in 0..250 {
            if let Some(answer) =
                runtime::take_answer(&project, "login", "implementer", &taken).unwrap()
            {
                runtime::close_dialog(&project, "login", "implementer", &answer.id, true).unwrap();
                return Some(answer);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    });
    let bearer = format!("Bearer {token}");
    let body = serde_json::json!({"id": id, "choice": "deny", "message": "dry-run first"});
    let body = body.to_string();
    let handled = route(
        &f.config,
        VAPID,
        &request("POST", &agent, "", Some(&bearer), &body),
    );
    let answer = hook.join().unwrap().unwrap();
    assert_eq!(handled.reply.status(), 200);
    assert_eq!(answer.choice, "deny");
    assert_eq!(answer.message.as_deref(), Some("dry-run first"));
    let logged = handled.detail.unwrap();
    assert!(
        logged.starts_with("dialog deny message sha256:"),
        "{logged}"
    );
    assert!(!logged.contains("dry-run"), "{logged}");
    assert_eq!(
        get_all(),
        std::slice::from_ref(&older.dialog.id),
        "answered"
    );
    let again = post(&serde_json::json!({"id": id, "choice": "allow"}));
    assert_eq!(again.0, 409);
    assert!(again.1.contains(r#""refused":"answered""#), "{}", again.1);
}
