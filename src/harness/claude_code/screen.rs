//! Reading a dialog that no hook reports off Claude Code's screen, captured
//! with its escape sequences (verified on 2.1.296). Such a dialog is drawn
//! beside the input box ([`super::input`]) or in its place: above the box,
//! its footer on the box's upper rule; below it, or in its place, its
//! footer the screen's last line. The footer names `Esc to cancel`, and
//! above it the dialog's selected option starts with `❯`. Its top border, a
//! rule of `─` or `▔`, is followed by its heading, the title read here; a
//! dialog taller than the screen has scrolled both away.
//!
//! The signature is the footer's place, not the box's state, so a draft, vim
//! NORMAL mode, a `/` menu or a busy spinner reads as no dialog. A picker
//! the user opened that has such a footer (`/resume`, the setup form
//! `/auto-mode-setup` opens) reads as one.

use std::time::Duration;

use crate::harness::screen::visible_text;

/// How long after a turn ends a dialog no hook reports can take to be
/// drawn: the Teach offer opens as the turn ends ([`super::waiting`]), and
/// no other opens between turns unreported, the setup proposal firing a
/// notification.
pub(in crate::harness) const OPENS_WITHIN: Duration = Duration::from_secs(10);

const FOOTER: &str = "Esc to cancel";
const SELECTED: char = '❯';
const RULES: [char; 2] = ['─', '▔'];

/// A dialog drawn on the screen.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::harness) struct Shown {
    /// Its heading; `None` once scrolled off the screen.
    pub title: Option<String>,
}

/// The dialog `screen` shows beside or in place of the input box; `None`
/// when it shows none.
pub(in crate::harness) fn dialog(screen: &str) -> Option<Shown> {
    let lines: Vec<String> = screen.lines().map(visible_text).collect();
    let text = |i: usize| lines[i].trim();
    let is_rule = |i: usize| text(i).starts_with(RULES);
    let last = lines.iter().rposition(|line| !line.trim().is_empty())?;
    let footer = if text(last).contains(FOOTER) {
        last
    } else {
        let prompt = lines
            .iter()
            .rposition(|line| line.trim_start().starts_with(SELECTED))?;
        let footer = prompt.checked_sub(2)?;
        if !is_rule(prompt - 1) || !text(footer).contains(FOOTER) {
            return None;
        }
        footer
    };
    let border = (0..footer).rev().find(|&i| is_rule(i));
    let body = border.map_or(0, |b| b + 1)..footer;
    if !body.clone().any(|i| text(i).starts_with(SELECTED)) {
        return None;
    }
    let title = border.and_then(|_| {
        body.map(text)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    });
    Some(Shown { title })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        crate::testing::screen_fixture(&format!("claude-code-{name}"))
    }

    fn titled(title: &str) -> Option<Shown> {
        Some(Shown {
            title: Some(title.into()),
        })
    }

    #[test]
    fn the_teach_offer_reads_as_a_dialog_above_or_below_the_box() {
        let title = "Teach auto mode about your environment?";
        assert_eq!(dialog(&fixture("teach-below")), titled(title));
        assert_eq!(dialog(&fixture("teach-above")), titled(title));
        assert_eq!(dialog(&fixture("setup-form")), titled(title));
    }

    #[test]
    fn a_proposal_taller_than_the_screen_reads_as_an_untitled_dialog() {
        assert_eq!(dialog(&fixture("proposal")), Some(Shown { title: None }));
    }

    #[test]
    fn a_box_at_its_prompt_or_at_work_reads_as_no_dialog() {
        for name in ["busy", "draft", "scanning"] {
            assert_eq!(dialog(&fixture(name)), None, "{name}");
        }
        let rule = "\x1b[38;5;244m──────────────────────────────";
        let normal = format!("⏺ Done.\n\n{rule}\n❯\u{a0}\n{rule}\n  ⏸ manual mode on\n");
        assert_eq!(dialog(&normal), None);
        let draft = format!("{rule}\n❯\u{a0}press Esc to cancel\n{rule}\n  status\n");
        assert_eq!(dialog(&draft), None);
        let said = format!("⏺ Press Esc to cancel.\n\n{rule}\n❯\u{a0}\n{rule}\n  status\n");
        assert_eq!(dialog(&said), None);
        assert_eq!(dialog(""), None);
    }
}
