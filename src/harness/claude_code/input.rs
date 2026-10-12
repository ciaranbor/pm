//! Reading Claude Code's input box off a screen captured with its escape
//! sequences (verified on 2.1.287). The box is one line between two
//! horizontal rules (the upper one may carry the agent's name), starting
//! with `❯` and a no-break space; a draft that wraps or spans lines makes it
//! taller. A fresh session's placeholder (`Try "…"`) is drawn dim, so dim
//! text doesn't count as a draft. With `editorMode: "vim"`, typed keys are
//! commands in NORMAL mode, which shows no indicator, so the box counts as
//! empty only while `-- INSERT --` shows; `i` there enters INSERT mode and
//! changes nothing else ([`text_mode_key`]).
//!
//! `editorMode` is read from the user settings (`settings.json` in
//! `$CLAUDE_CONFIG_DIR`, else in `~/.claude`), which decide when they set
//! it, else from the global config (`.claude.json` in `$CLAUDE_CONFIG_DIR`,
//! else in the home directory). Project and local settings outrank both
//! (verified on 2.1.296) but are not read here.
//!
//! Anything else in the box's place — a dialog — reads as unknown, so a
//! caller that types only into an empty box errs towards leaving it alone.

use std::path::Path;

use crate::harness::screen::visible_text;

const PROMPT: char = '❯';
const RULE: char = '─';
const INSERT: &str = "-- INSERT --";

/// The variable naming the agent's config dir, which holds `.claude.json` and
/// `settings.json`.
pub(in crate::harness) const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Whether the input box at the bottom of `screen` holds no text; `None`
/// when no input box is found or typed keys might not reach it as text.
/// `config_dir` is the agent's [`CONFIG_DIR_ENV`], if it set one.
pub(in crate::harness) fn is_empty(
    screen: &str,
    home: &Path,
    config_dir: Option<&Path>,
) -> Option<bool> {
    is_empty_in(screen, vim_mode(home, config_dir))
}

/// The key that puts an input box in vim NORMAL mode into INSERT mode, and
/// the one that erases it should the box have taken it as text after all;
/// `None` when the box is not an empty one in NORMAL mode, so a draft is
/// left alone, mode included.
pub(in crate::harness) fn text_mode_key(
    screen: &str,
    home: &Path,
    config_dir: Option<&Path>,
) -> Option<(&'static str, &'static str)> {
    let vim = vim_mode(home, config_dir);
    (vim && is_empty_in(screen, false) == Some(true) && is_empty_in(screen, true).is_none())
        .then_some(("i", "BSpace"))
}

/// The keys that interrupt a turn: Escape, or with the input box in vim
/// INSERT mode, one Escape to leave it and another to interrupt (verified
/// on 2.1.296). A further Escape, at the prompt, opens the rewind menu.
/// Read off the screen: only it shows whether the box is in INSERT mode. A
/// dialog over the box takes the one Escape.
pub(in crate::harness) fn interrupt_keys(screen: &str) -> &'static [&'static str] {
    if is_empty_in(screen, true).is_some() {
        &["Escape", "Escape"]
    } else {
        &["Escape"]
    }
}

fn is_empty_in(screen: &str, vim: bool) -> Option<bool> {
    let lines: Vec<String> = screen.lines().map(visible_text).collect();
    let prompt = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with(PROMPT))?;
    let is_rule = |i: usize| {
        lines
            .get(i)
            .is_some_and(|line| line.trim_start().starts_with(RULE))
    };
    if prompt == 0 || !is_rule(prompt - 1) {
        return None;
    }
    if vim && !lines[prompt..].iter().any(|line| line.contains(INSERT)) {
        return None;
    }
    if !is_rule(prompt + 1) {
        return Some(false);
    }
    let rest = lines[prompt].trim_start().trim_start_matches(PROMPT);
    Some(rest.trim().is_empty())
}

/// Whether Claude Code's config turns on its vim editor mode.
fn vim_mode(home: &Path, config_dir: Option<&Path>) -> bool {
    let global = config_dir.unwrap_or(home).join(".claude.json");
    let settings = config_dir.map_or_else(|| home.join(".claude"), Path::to_path_buf);
    let editor_mode = |file: &Path| {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|config| config["editorMode"].as_str().map(str::to_string))
    };
    editor_mode(&settings.join("settings.json")).or_else(|| editor_mode(&global))
        == Some("vim".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(input: &[&str], status: &str) -> String {
        let mut lines = vec![
            "⏺ Done.",
            "",
            "\x1b[38;5;244m───────────────────────────── reviewer ─",
        ];
        lines.extend_from_slice(input);
        lines.push("\x1b[38;5;244m────────────────────────────────────────\x1b[39m");
        lines.push(status);
        lines.join("\n")
    }

    fn input(line: &str) -> String {
        screen(&[line], "  ⏸ manual mode on")
    }

    #[test]
    fn an_empty_box_reads_empty_and_a_draft_does_not() {
        assert_eq!(is_empty_in(&input("\x1b[39m❯\u{a0}"), false), Some(true));
        assert_eq!(is_empty_in(&input("❯ "), false), Some(true));
        assert_eq!(
            is_empty_in(&input("\x1b[39m❯\u{a0}half a thought"), false),
            Some(false)
        );
        let two_lines = screen(&["❯\u{a0}first line", "  second"], "");
        assert_eq!(is_empty_in(&two_lines, false), Some(false));
    }

    #[test]
    fn a_dim_placeholder_is_not_a_draft() {
        let placeholder = input("\x1b[39m❯\u{a0}\x1b[2mTry \"fix lint errors\"\x1b[0m");
        assert_eq!(is_empty_in(&placeholder, false), Some(true));
        let after = input("❯\u{a0}\x1b[2mTry\x1b[22m typed");
        assert_eq!(is_empty_in(&after, false), Some(false));
    }

    #[test]
    fn in_vim_mode_only_insert_mode_reads_empty() {
        let insert = screen(&["❯\u{a0}"], "  -- INSERT -- ⏸ manual mode on");
        assert_eq!(is_empty_in(&insert, true), Some(true));
        assert_eq!(is_empty_in(&input("❯\u{a0}"), true), None);
    }

    #[test]
    fn no_input_box_is_unknown() {
        assert_eq!(is_empty_in("", false), None);
        let dialog = " Security guide\n ❯ No, exit\n   Yes, I trust this folder\n";
        assert_eq!(is_empty_in(dialog, false), None);
    }

    #[test]
    fn only_vim_normal_mode_takes_a_key_to_reach_insert_mode() {
        let vim = tempfile::tempdir().unwrap();
        std::fs::write(vim.path().join(".claude.json"), r#"{"editorMode":"vim"}"#).unwrap();
        let plain = tempfile::tempdir().unwrap();
        let normal = input("❯\u{a0}");
        let insert = screen(&["❯\u{a0}"], "  -- INSERT -- ⏸ manual mode on");
        let dialog = " Security guide\n ❯ No, exit\n   Yes, I trust this folder\n";

        assert_eq!(
            text_mode_key(&normal, plain.path(), Some(vim.path())),
            Some(("i", "BSpace"))
        );
        assert_eq!(text_mode_key(&normal, vim.path(), Some(plain.path())), None);
        assert_eq!(
            text_mode_key(&normal, vim.path(), None),
            Some(("i", "BSpace"))
        );
        assert_eq!(text_mode_key(&insert, vim.path(), None), None);
        assert_eq!(text_mode_key(dialog, vim.path(), None), None);
        let draft = input("❯\u{a0}half a thought");
        assert_eq!(text_mode_key(&draft, vim.path(), None), None);
    }

    #[test]
    fn insert_mode_takes_an_escape_more_to_interrupt() {
        let insert = screen(&["❯\u{a0}"], "  -- INSERT -- ⏸ manual mode on");
        let draft = screen(
            &["❯\u{a0}half a thought"],
            "  -- INSERT -- esc to interrupt",
        );
        let dialog = " Do you want to proceed?\n ❯ 1. Yes\n   2. No\n";

        assert_eq!(interrupt_keys(&insert), ["Escape", "Escape"]);
        assert_eq!(interrupt_keys(&draft), ["Escape", "Escape"]);
        assert_eq!(interrupt_keys(&input("❯\u{a0}")), ["Escape"]);
        assert_eq!(interrupt_keys(dialog), ["Escape"]);
    }

    #[test]
    fn the_user_settings_decide_vim_mode_over_the_global_config() {
        let vim = r#"{"editorMode":"vim"}"#;
        let home = tempfile::tempdir().unwrap();
        assert!(!vim_mode(home.path(), None));
        std::fs::write(home.path().join(".claude.json"), vim).unwrap();
        assert!(vim_mode(home.path(), None));

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/settings.json"), vim).unwrap();
        assert!(vim_mode(home.path(), None));

        let config_dir = tempfile::tempdir().unwrap();
        assert!(!vim_mode(home.path(), Some(config_dir.path())));
        std::fs::write(config_dir.path().join("settings.json"), vim).unwrap();
        assert!(vim_mode(home.path(), Some(config_dir.path())));

        let normal = r#"{"editorMode":"normal"}"#;
        std::fs::write(home.path().join(".claude.json"), vim).unwrap();
        std::fs::write(home.path().join(".claude/settings.json"), normal).unwrap();
        assert!(!vim_mode(home.path(), None));
        std::fs::write(home.path().join(".claude.json"), normal).unwrap();
        std::fs::write(home.path().join(".claude/settings.json"), vim).unwrap();
        assert!(vim_mode(home.path(), None));
    }
}
