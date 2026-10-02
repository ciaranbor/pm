//! Reading Claude Code's input box off a screen captured with its escape
//! sequences (verified on 2.1.287). The box is one line between two
//! horizontal rules (the upper one may carry the agent's name), starting
//! with `❯` and a no-break space; a draft that wraps or spans lines makes it
//! taller. A fresh session's placeholder (`Try "…"`) is drawn dim, so dim
//! text doesn't count as a draft. With `editorMode: "vim"`, typed keys are
//! commands in NORMAL mode, which shows no indicator, so the box counts as
//! empty only while `-- INSERT --` shows. Anything else in the box's place —
//! a dialog — reads as unknown, so a caller that types only into an empty
//! box errs towards leaving it alone.

use std::path::Path;

use crate::harness::screen::visible_text;

const PROMPT: char = '❯';
const RULE: char = '─';
const INSERT: &str = "-- INSERT --";

/// Whether the input box at the bottom of `screen` holds no text; `None`
/// when no input box is found or typed keys might not reach it as text.
pub(in crate::harness) fn is_empty(screen: &str, home: &Path) -> Option<bool> {
    is_empty_in(screen, vim_mode(home))
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

/// Whether Claude Code's global config turns on its vim editor mode.
fn vim_mode(home: &Path) -> bool {
    std::fs::read_to_string(home.join(".claude.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|config| config["editorMode"] == "vim")
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
    fn vim_mode_is_read_from_the_global_config() {
        let home = tempfile::tempdir().unwrap();
        assert!(!vim_mode(home.path()));
        std::fs::write(home.path().join(".claude.json"), r#"{"editorMode":"vim"}"#).unwrap();
        assert!(vim_mode(home.path()));
    }
}
