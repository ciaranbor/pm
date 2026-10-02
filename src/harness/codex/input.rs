//! Reading codex's composer off a screen captured with its escape sequences
//! (verified on 0.160). The composer starts at a line opening with a bold
//! `›`, its placeholder (`Ask Codex to do anything`) drawn dim, and a blank
//! line follows it; a draft that spans lines fills the lines below. A
//! selection list — an approval or trust prompt — marks its current option
//! with the same `›` followed by a number, so a numbered line reads as
//! unknown, as does a screen with no `›` line.

use crate::harness::screen::visible_text;

const PROMPT: char = '›';

/// Whether the composer at the bottom of `screen` holds no text; `None`
/// when no composer is found.
pub(in crate::harness) fn is_empty(screen: &str) -> Option<bool> {
    let lines: Vec<String> = screen.lines().map(visible_text).collect();
    let prompt = lines.iter().rposition(|line| line.starts_with(PROMPT))?;
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
}
