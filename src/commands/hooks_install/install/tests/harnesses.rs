use super::*;

#[test]
fn install_writes_the_opencode_plugin_and_replaces_a_stale_copy() {
    let (_dir, home, _root) = setup();
    assert!(!is_installed_in(Harness::OpenCode, &home).unwrap());
    let dry = install_in(&home, None, true).unwrap();
    assert!(
        dry.contains(&format!(
            "Would install pm plugin in {}",
            plugin_dir(&home).display()
        )),
        "{dry:?}"
    );
    assert!(!plugin_dir(&home).exists(), "dry-run wrote the plugin");

    install_in(&home, None, false).unwrap();
    assert!(is_installed_in(Harness::OpenCode, &home).unwrap());
    let index = plugin_dir(&home).join("index.ts");
    let bundled = fs::read_to_string(&index).unwrap();
    // The entry point resolves its import beside itself.
    assert!(plugin_dir(&home).join("loop.ts").is_file());
    assert!(install_in(&home, None, false).unwrap().is_empty());

    // An older release's copy, and a user's own plugin beside pm's.
    fs::write(&index, "export default {}").unwrap();
    let theirs = home.join(".config/opencode/plugins/mine/index.ts");
    fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    fs::write(&theirs, "export default { id: 'mine' }").unwrap();
    assert!(!is_installed_in(Harness::OpenCode, &home).unwrap());
    assert_eq!(
        stale_plugin_files(Harness::OpenCode, &home),
        vec![index.clone()]
    );

    let lines = install_in(&home, None, false).unwrap();
    assert_eq!(
        lines,
        vec![format!(
            "Installed pm plugin in {}",
            plugin_dir(&home).display()
        )]
    );
    assert_eq!(fs::read_to_string(&index).unwrap(), bundled);
    assert_eq!(
        fs::read_to_string(&theirs).unwrap(),
        "export default { id: 'mine' }"
    );
}

#[test]
fn install_appends_after_a_codex_users_own_hooks() {
    let (_dir, home, root) = setup();
    // A codex user with a hook of their own, at index 0.
    let codex_hooks = codex_file(&home);
    write_json(
        &codex_hooks,
        &json!({"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "log prompt"}]}]
        }}),
    );

    let lines = install_in(&home, Some(&root), false).unwrap();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[1].ends_with(&codex_hooks.display().to_string()),
        "{lines:?}"
    );
    assert!(is_installed_in(Harness::Codex, &home).unwrap());
    assert!(claude_installed_in(&home).unwrap());

    let parsed = read_json(&codex_hooks);
    // The user's hook keeps position 0 (codex keys its trust on the
    // index); pm's entry follows.
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
        "say done"
    );
    assert_eq!(
        pm_hook_position(&parsed, "Stop", STOP_MARKERS),
        Some((1, 0))
    );
    assert_eq!(
        pm_hook_position(&parsed, "SessionStart", SESSION_START_MARKERS),
        Some((0, 0))
    );
    assert_eq!(
        pm_hook_position(&parsed, "UserPromptSubmit", USER_PROMPT_MARKERS),
        Some((1, 0))
    );
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/1/hooks/0/command"),
        stop_hook_command(Harness::Codex)
    );
    assert_eq!(
        parsed
            .pointer("/hooks/Stop/1/hooks/0/timeout")
            .and_then(|v| v.as_u64()),
        Some(STOP_HOOK_TIMEOUT_SECS)
    );
    assert!(Harness::Codex.malformed_hook_events(&parsed).is_empty());

    // Idempotent across both files.
    assert!(install_in(&home, Some(&root), true).unwrap().is_empty());
}
