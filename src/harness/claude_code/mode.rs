//! Claude Code's permission mode, read off its screen and changed with
//! ⇧Tab (verified on 2.1.296). ⇧Tab is the only way to change a running
//! interactive session's mode: no command, setting or statusline field
//! does, and a PermissionRequest hook's `setMode` applies only while its
//! dialog is up. ⇧Tab works mid-turn.
//!
//! The mode shows in the footer, a row below the input box's bottom rule,
//! with a statusline row or notices possibly between them: `⏸ plan mode on
//! (shift+tab to cycle)`, after `-- INSERT --` in vim INSERT mode (NORMAL
//! mode shows no prefix, and ⇧Tab cycles there too). The UI calls `default`
//! "Manual"; hook payloads still say `default`. A width or a notice can push
//! the row off, and then the mode reads as unknown rather than guessed.
//!
//! ⇧Tab cycles auto → manual → accept edits → plan → auto; bypass follows
//! plan with `--allow-dangerously-skip-permissions`, and auto drops out
//! where the account lacks it. `dontAsk` is never in the cycle: a session
//! that leaves it can't come back. A plain launch starts in auto (since
//! 2.1.283).
//!
//! While a dialog is up the mode row is gone and ⇧Tab acts on the dialog:
//! on a file-write prompt it approves the write and switches to accept
//! edits, on a Bash prompt it toggles the comment field. So a screen with
//! no mode row is never pressed on. A dialog drawn under the box, as "Teach
//! auto mode about your environment?" is after a turn, starts with a rule
//! of its own and hides the row the same way.

use crate::harness::screen::plain_text;

/// The key that moves the session to the next mode of its cycle.
pub(in crate::harness) const CYCLE_KEY: &str = "BTab";

/// The modes a device may pick, by the ids hook payloads use, with the
/// UI's names for them.
pub(in crate::harness) const MODES: &[(&str, &str)] = &[
    ("default", "Manual"),
    ("acceptEdits", "Accept edits"),
    ("plan", "Plan"),
    ("auto", "Auto"),
    ("bypassPermissions", "Bypass"),
];

/// Each footer phrase, lowercased, and the mode it shows.
const FOOTER: &[(&str, &str)] = &[
    ("manual mode on", "default"),
    ("accept edits on", "acceptEdits"),
    ("plan mode on", "plan"),
    ("auto mode on", "auto"),
    ("bypass permissions on", "bypassPermissions"),
    ("don't ask on", "dontAsk"),
    ("don’t ask on", "dontAsk"),
];

const RULE: char = '─';

/// The mode the footer of `screen`, captured with its escape sequences,
/// shows; `None` when no footer below the input box shows one.
pub(in crate::harness) fn on_screen(screen: &str) -> Option<&'static str> {
    let lines: Vec<String> = screen.lines().map(plain_text).collect();
    let rule = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with(RULE))?;
    lines[rule + 1..].iter().find_map(|line| {
        let line = line.to_lowercase();
        FOOTER
            .iter()
            .find(|(phrase, _)| line.contains(phrase))
            .map(|&(_, mode)| mode)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE_LINE: &str = "\x1b[38;5;244m────────────────────────────────────────\x1b[39m";

    fn screen(footer: &[&str]) -> String {
        let mut lines = vec![
            "⏺ Done.",
            "",
            "\x1b[38;5;244m───────────────────────────── reviewer ─",
            "\x1b[39m❯\u{a0}",
            RULE_LINE,
        ];
        lines.extend_from_slice(footer);
        lines.join("\n")
    }

    #[test]
    fn each_footer_phrase_reads_as_its_mode() {
        let cases = [
            ("  ⏸ manual mode on", "default"),
            (
                "  \x1b[38;5;114m⏵⏵ accept edits on\x1b[39m \x1b[2m(shift+tab to cycle)\x1b[22m",
                "acceptEdits",
            ),
            (
                "  \x1b[38;5;66m⏸ plan mode on\x1b[39m (shift+tab to cycle)",
                "plan",
            ),
            ("  ⏵⏵ auto mode on (shift+tab to cycle)", "auto"),
            (
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
                "bypassPermissions",
            ),
            ("  ⏵⏵ don't ask on", "dontAsk"),
        ];
        for (footer, mode) in cases {
            assert_eq!(on_screen(&screen(&[footer])), Some(mode), "{footer}");
        }
    }

    #[test]
    fn a_dim_footer_and_a_vim_insert_prefix_still_read() {
        let insert = "  -- INSERT -- ⏸ plan mode on (shift+tab to cycle) · ← for agents";
        assert_eq!(on_screen(&screen(&[insert])), Some("plan"));
        let dim = "  \x1b[2m⏸ manual mode on\x1b[22m";
        assert_eq!(on_screen(&screen(&[dim])), Some("default"));
    }

    #[test]
    fn a_statusline_row_between_the_rule_and_the_mode_row_is_skipped() {
        let footer = ["  main · opus · 41% context", "  ⏸ plan mode on"];
        assert_eq!(on_screen(&screen(&footer)), Some("plan"));
    }

    #[test]
    fn a_dialog_reads_as_no_mode() {
        let permission = [
            "⏺ Write(notes.md)",
            RULE_LINE,
            " Do you want to create notes.md?",
            " ❯ 1. Yes",
            "   2. Yes, allow all edits during this session (shift+tab)",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]
        .join("\n");
        assert_eq!(on_screen(&permission), None);

        let teach = [
            "\x1b[38;5;153m──────────────────────────────",
            "  Teach auto mode about your environment?",
            "  ❯ 1. Yes",
            "    2. Not now",
        ];
        let mut below = vec!["  ⏵⏵ auto mode on (shift+tab to cycle)", ""];
        below.extend_from_slice(&teach);
        assert_eq!(on_screen(&screen(&below)), None);
    }

    #[test]
    fn a_screen_without_a_footer_reads_as_no_mode() {
        assert_eq!(on_screen(""), None);
        assert_eq!(on_screen(&screen(&["  ? for shortcuts"])), None);
        assert_eq!(on_screen("plan mode on"), None);
    }
}
