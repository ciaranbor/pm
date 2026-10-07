//! `install` end to end: a fresh machine, what it leaves alone (the project
//! config, the user's own settings), repeat runs, and a malformed file.

mod harnesses;
mod upgrade;

use super::*;
use crate::commands::hooks_install::entries::{
    SESSION_START_MARKERS, STOP_MARKERS, USER_PROMPT_MARKERS, WAITING_MARKERS,
};
use crate::commands::hooks_install::test_support::*;
use crate::commands::hooks_install::*;
use serde_json::json;
use std::fs;

#[test]
fn install_creates_user_file_on_fresh_machine() {
    let (_dir, home, root) = setup();
    assert!(!home.exists());

    let lines = install_in(&home, Some(&root), false).unwrap();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("Installed pm hooks in "), "{lines:?}");
    assert!(
        lines[1].ends_with(&codex_file(&home).display().to_string()),
        "{lines:?}"
    );

    let parsed = read_json(&user_file(&home));
    assert_eq!(parsed["hooks"]["Stop"].as_array().unwrap().len(), 1);
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
    assert_eq!(
        command_at(&parsed, "/hooks/UserPromptSubmit/0/hooks/0/command"),
        user_prompt_hook_command()
    );
    assert!(claude_installed_in(&home).unwrap());
    // Every supported harness, whether or not it is installed or
    // configured anywhere: a project may name it later.
    assert!(is_installed_in(Harness::Codex, &home).unwrap());
    let codex = read_json(&codex_file(&home));
    let events = |root: &Value| -> Vec<String> {
        let mut events: Vec<String> = root["hooks"].as_object().unwrap().keys().cloned().collect();
        events.sort();
        events
    };
    assert_eq!(
        events(&parsed),
        [
            "Notification",
            "PermissionRequest",
            "PostToolUse",
            "PostToolUseFailure",
            "SessionStart",
            "Stop",
            "StopFailure",
            "UserPromptSubmit"
        ]
    );
    assert_eq!(
        events(&codex),
        [
            "Interrupt",
            "PermissionRequest",
            "PostToolUse",
            "PreToolUse",
            "SessionStart",
            "Stop",
            "UserPromptSubmit"
        ]
    );
    assert_eq!(
        command_at(&codex, "/hooks/Interrupt/0/hooks/0/command"),
        waiting_hook_command(Harness::Codex)
    );
    assert_eq!(
        command_at(&parsed, "/hooks/StopFailure/0/hooks/0/command"),
        waiting_hook_command(Harness::ClaudeCode)
    );
    // Each runs the Stop hook as its waiter, once the turn has ended.
    assert_eq!(
        parsed.pointer("/hooks/Stop/0/hooks/0/asyncRewake"),
        Some(&json!(true))
    );
    assert_eq!(
        command_at(&codex, "/hooks/Stop/0/hooks/0/command"),
        stop_hook_command(Harness::Codex)
    );
    assert_eq!(
        codex.pointer("/hooks/Stop/0/hooks/0/async"),
        Some(&json!(true))
    );
    // The dialog hook blocks, so it gets an entry of its own beside the
    // status hook's, which must answer at once, and the Stop hook's
    // timeout; codex gets none.
    assert_eq!(
        parsed["hooks"]["PermissionRequest"],
        json!([
            {"hooks": [{"type": "command", "command": waiting_hook_command(Harness::ClaudeCode)}]},
            {"hooks": [{"type": "command", "command": dialog_hook_command(Harness::ClaudeCode),
                        "timeout": STOP_HOOK_TIMEOUT_SECS}]}
        ])
    );
    assert_eq!(
        codex["hooks"]["PermissionRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A fresh project gets no project-level file.
    assert!(!paths::main_worktree(&root).join(".claude").exists());
    assert_eq!(
        lines[2],
        format!("Installed pm plugin in {}", plugin_dir(&home).display())
    );
}

#[test]
fn install_ignores_the_project_config() {
    // Which harnesses a project uses is not consulted, so a malformed
    // config can't block the install or narrow it.
    let (_dir, home, root) = setup();
    let pm_dir = root.join(".pm");
    fs::create_dir_all(&pm_dir).unwrap();
    fs::write(
        pm_dir.join("config.toml"),
        "[agents.harness]\n[agents.harness]\n",
    )
    .unwrap();

    let lines = install_in(&home, Some(&root), false).unwrap();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(claude_installed_in(&home).unwrap());
    assert!(is_installed_in(Harness::Codex, &home).unwrap());
}

#[test]
fn install_preserves_the_users_own_settings_and_hooks() {
    let (_dir, home, _root) = setup();
    write_json(
        &user_file(&home),
        &json!({
            "model": "sonnet",
            "tui": {"theme": "dark"},
            "permissions": {"allow": ["Bash(ls:*)"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "echo user stop"}]}],
                "SessionStart": [{"hooks": [{"type": "command", "command": "echo user start"}]}]
            }
        }),
    );

    install_in(&home, None, false).unwrap();

    let parsed = read_json(&user_file(&home));
    assert_eq!(parsed["model"], "sonnet");
    assert_eq!(parsed["tui"]["theme"], "dark");
    assert_eq!(parsed["permissions"]["allow"][0], "Bash(ls:*)");
    let stop = parsed["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2);
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/0/hooks/0/command"),
        "echo user stop"
    );
    assert_eq!(
        command_at(&parsed, "/hooks/Stop/1/hooks/0/command"),
        stop_hook_command(Harness::ClaudeCode)
    );
    let ss = parsed["hooks"]["SessionStart"].as_array().unwrap();
    assert_eq!(ss.len(), 2);
    assert_eq!(
        command_at(&parsed, "/hooks/SessionStart/0/hooks/0/command"),
        "echo user start"
    );
}

#[test]
fn install_is_idempotent() {
    let (_dir, home, root) = setup();
    install_in(&home, Some(&root), false).unwrap();
    let first = fs::read_to_string(user_file(&home)).unwrap();

    let lines = install_in(&home, Some(&root), false).unwrap();
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(fs::read_to_string(user_file(&home)).unwrap(), first);
    assert!(install_dry_run_in(&home, &root).is_empty());
}

#[test]
fn install_refuses_a_malformed_user_file() {
    let (_dir, home, _root) = setup();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(user_file(&home), "[]").unwrap();
    assert!(install_in(&home, None, false).is_err());
    assert_eq!(fs::read_to_string(user_file(&home)).unwrap(), "[]");
}
