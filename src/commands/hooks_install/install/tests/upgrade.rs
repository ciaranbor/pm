use super::*;

#[test]
fn install_rewrites_older_user_level_spellings_in_place() {
    for old in [
        "pm harness hooks stop",
        "[ -n \"$PM_AGENT_NAME\" ] || exit 0; pm harness hooks stop",
    ] {
        let (_dir, home, _root) = setup();
        write_json(
            &user_file(&home),
            &json!({"hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": old}]}],
                "SessionStart": [{"hooks": [{"type": "command", "command": old.replace("stop", "session-start")}]}],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": user_prompt_hook_command()}]}]
            }}),
        );
        assert!(claude_installed_in(&home).unwrap(), "{old}");

        let lines = install_in(&home, None, false).unwrap();
        assert_eq!(lines.len(), 3, "{old}: {lines:?}");
        assert!(lines[0].ends_with(&user_file(&home).display().to_string()));

        let parsed = read_json(&user_file(&home));
        assert_eq!(
            parsed["hooks"]["Stop"].as_array().unwrap().len(),
            1,
            "{old}"
        );
        assert_eq!(
            command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
            stop_hook_command(Harness::ClaudeCode)
        );
        assert_eq!(
            parsed
                .pointer("/hooks/Stop/0/hooks/0/timeout")
                .and_then(|v| v.as_u64()),
            Some(STOP_HOOK_TIMEOUT_SECS)
        );
        assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(
            command_at(&parsed, "/hooks/SessionStart/0/hooks/0/command"),
            session_start_hook_command()
        );
    }
}

#[test]
fn an_upgrade_adds_the_status_hooks_and_lengthens_the_stop_timeout_in_place() {
    let (_dir, home, _root) = setup();
    let hook = |command: String| json!({"hooks": [{"type": "command", "command": command}]});
    write_json(
        &codex_file(&home),
        &json!({"hooks": {
            "PostToolUse": [hook("log tool".into())],
            "Stop": [{"hooks": [{"type": "command", "command": stop_hook_command(Harness::ClaudeCode), "timeout": 86400}]}],
            "SessionStart": [hook(session_start_hook_command())],
            "UserPromptSubmit": [hook(user_prompt_hook_command())]
        }}),
    );
    let before = read_json(&codex_file(&home));
    assert_eq!(
        missing_status_hooks(Harness::Codex, Some(&before)),
        [
            "PermissionRequest",
            "PreToolUse",
            "PostToolUse",
            "Interrupt"
        ]
    );

    install_in(&home, None, false).unwrap();

    let parsed = read_json(&codex_file(&home));
    assert!(missing_status_hooks(Harness::Codex, Some(&parsed)).is_empty());
    assert_eq!(
        parsed.pointer("/hooks/Stop/0/hooks/0/timeout"),
        Some(&json!(STOP_HOOK_TIMEOUT_SECS))
    );
    assert_eq!(parsed["hooks"]["Stop"].as_array().unwrap().len(), 1);
    // The user's own entry keeps index 0, which codex keys trust on.
    assert_eq!(
        command_at(&parsed, "/hooks/PostToolUse/0/hooks/0/command"),
        "log tool"
    );
    assert_eq!(
        pm_hook_position(&parsed, "PostToolUse", WAITING_MARKERS),
        Some((1, 0))
    );
    assert!(install_in(&home, None, false).unwrap().is_empty());
}

#[test]
fn foreign_msg_wait_hook_is_not_pm_owned() {
    // The two-generations-old `pm msg wait` shape is no longer claimed:
    // a user's own hook wrapping it is left alone and not counted.
    let (_dir, home, _root) = setup();
    write_json(
        &user_file(&home),
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "pm msg wait && notify"}]}]}}),
    );
    assert!(!claude_installed_in(&home).unwrap());

    install_in(&home, None, false).unwrap();
    let parsed = read_json(&user_file(&home));
    let stop = parsed["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2);
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
        "pm msg wait && notify"
    );
}

#[test]
fn install_keeps_a_foreign_hook_bundled_with_pms() {
    let (_dir, home, _root) = setup();
    write_json(
        &user_file(&home),
        &json!({"hooks": {"Stop": [{"matcher": "x", "hooks": [
            {"type": "command", "command": "pm harness hooks stop"},
            {"type": "command", "command": "my-notify"}
        ]}]}}),
    );

    install_in(&home, None, false).unwrap();

    let parsed = read_json(&user_file(&home));
    let stop = parsed["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1, "{parsed}");
    assert_eq!(stop[0]["matcher"], "x");
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
        stop_hook_command(Harness::ClaudeCode)
    );
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/0/hooks/1/command"),
        "my-notify"
    );
}
