//! Reading codex's composer off a screen captured with its escape sequences
//! (verified on 0.160). The composer starts at a line opening with a bold
//! `›`, its placeholder (`Ask Codex to do anything`) drawn dim, and a blank
//! line follows it; a draft that spans lines fills the lines below. A
//! selection list — an approval or trust prompt — marks its current option
//! with the same `›` followed by a number, so a numbered line reads as
//! unknown, as does a screen with no `›` line.
//!
//! The vim composer (`/vim`, `tui.vim_mode_default`) shows `Vim: Normal` or
//! `Vim: Insert` in the footer. In NORMAL mode typed keys are commands, so
//! the composer reads as unknown; `i` there enters INSERT mode and changes
//! nothing else ([`text_mode_key`]).

use crate::harness::screen::visible_text;

const PROMPT: char = '›';
const VIM_NORMAL: &str = "Vim: Normal";

/// Whether the composer at the bottom of `screen` holds no text; `None`
/// when no composer is found or typed keys would not reach it as text.
pub(in crate::harness) fn is_empty(screen: &str) -> Option<bool> {
    let (empty, normal) = composer(screen)?;
    if normal { None } else { empty }
}

/// The key that puts an empty composer in vim NORMAL mode into INSERT mode,
/// and the one that erases it should the composer have taken it as text;
/// `None` otherwise, so a draft is left alone, mode included.
pub(in crate::harness) fn text_mode_key(screen: &str) -> Option<(&'static str, &'static str)> {
    let (empty, normal) = composer(screen)?;
    (normal && empty == Some(true)).then_some(("i", "BSpace"))
}

/// The composer at the bottom of `screen`: whether it holds no text,
/// whatever its mode, and whether it is in vim NORMAL mode.
fn composer(screen: &str) -> Option<(Option<bool>, bool)> {
    let lines: Vec<String> = screen.lines().map(visible_text).collect();
    let prompt = lines.iter().rposition(|line| line.starts_with(PROMPT))?;
    let normal = lines[prompt..].iter().any(|line| line.contains(VIM_NORMAL));
    Some((composer_is_empty(&lines, prompt), normal))
}

fn composer_is_empty(lines: &[String], prompt: usize) -> Option<bool> {
    let rest = lines[prompt].trim_start_matches(PROMPT).trim();
    let numbered = rest
        .split_once(". ")
        .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if numbered {
        return None;
    }
    if !lines.get(prompt + 1)?.trim().is_empty() {
        return Some(false);
    }
    Some(rest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOOTER: &str = "\n  \x1b[38;2;246;226;183mGPT-6.1-Sol default\x1b[39m · /tmp/x\n\x1b[39m  \x1b[1m←\x1b[0m for agents · \x1b[1m?\x1b[0m for shortcuts\n";

    fn screen(composer: &str) -> String {
        format!(
            "■ Conversation interrupted - use /feedback if something went wrong\n\n\n{composer}\n{FOOTER}"
        )
    }

    #[test]
    fn a_placeholder_reads_empty_and_a_draft_does_not() {
        let placeholder = "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m";
        assert_eq!(is_empty(&screen(placeholder)), Some(true));
        assert_eq!(
            is_empty(&screen("\x1b[1m›\x1b[0m half a thought")),
            Some(false)
        );
        assert_eq!(
            is_empty(&screen("\x1b[1m›\x1b[0m half a thought\n  second line")),
            Some(false)
        );
    }

    #[test]
    fn a_selection_list_is_unknown() {
        let approval = "  \x1b[1mWould you like to run the following command?\x1b[0m\n\n\
                        \x1b[1;7m› 1. Yes, proceed (y)\n\
                        \x1b[0m  2. Yes, and don't ask again (\x1b[1mp\x1b[0m)\n\
                        \x1b[0m  3. No, and tell Codex what to do differently (\x1b[1mesc\x1b[0m)\n\n\
                        \x1b[2mPress \x1b[0;1menter\x1b[0;2m to confirm\n";
        assert_eq!(is_empty(approval), None);
        assert_eq!(
            is_empty("  Trust this folder?\n› 1. Trust and continue\n  2. Back\n"),
            None
        );
        assert_eq!(is_empty(""), None);
    }

    fn vim_screen(composer: &str, mode: &str) -> String {
        format!(
            "\n{composer}\n\n  \x1b[38;2;246;226;183mGPT-6.1-Sol default\x1b[39m · ~/scratch      \
             \x1b[38;5;5mVim: {mode}\x1b[39m\n  \x1b[1m?\x1b[0m for shortcuts\n"
        )
    }

    #[test]
    fn vim_normal_mode_is_unknown_until_i_enters_insert_mode() {
        let placeholder = "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m";
        let normal = vim_screen(placeholder, "Normal");
        assert_eq!(is_empty(&normal), None);
        assert_eq!(text_mode_key(&normal), Some(("i", "BSpace")));

        let insert = vim_screen(placeholder, "Insert");
        assert_eq!(is_empty(&insert), Some(true));
        assert_eq!(text_mode_key(&insert), None);

        let draft = vim_screen("\x1b[1m›\x1b[0m half a thought", "Normal");
        assert_eq!(is_empty(&draft), None);
        assert_eq!(text_mode_key(&draft), None);

        assert_eq!(text_mode_key(&screen(placeholder)), None);
    }
}
