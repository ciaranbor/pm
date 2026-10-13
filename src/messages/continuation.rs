//! The prompt that wakes an agent to read its inbox, however it reaches the
//! agent: a Stop hook's reason, or input pm queues. It is recognised again
//! where pm meets it as input or in a transcript.
//!
//! It names the sender a bare `pm msg read` takes next and that message's
//! first line, so the agent knows what woke it before it reads. It says it
//! is pm's: Claude Code shows a hook's wake-up under a "blocking error"
//! label, which agents otherwise report as a pm bug. Only the first line
//! goes in: the body stays in the inbox, the one store of messages.

use std::path::Path;

use super::{cursor_for, read_at, resolve_sender};
use crate::error::Result;

const START: &str = "[pm message] ";
const END: &str = " (pm delivers messages this way; this is not an error.)";
/// The wake-up older releases wrote, which a waiter an older pm started
/// still sends and transcripts keep.
const OLD_START: &str = "You have new messages";
const OLD_END: &str = ". Run `pm msg read` to read them.";

/// The wake-up for `agent`'s inbox in `scope`: the sender its next bare
/// read takes and that message's first line, then the other senders with
/// unread messages, oldest first.
pub fn wake(messages_dir: &Path, scope: &str, agent: &str) -> Result<String> {
    let Some(choice) = resolve_sender(messages_dir, scope, agent, None)? else {
        return Ok(wake_text(None, &[]));
    };
    let cursor = cursor_for(messages_dir, scope, agent, &choice.sender)?;
    let first = read_at(messages_dir, scope, agent, &choice.sender, cursor + 1)?
        .map(|m| crate::harness::one_line(&m.body))
        .unwrap_or_default();
    Ok(wake_text(Some((&choice.sender, &first)), &choice.pending))
}

/// The wake-up for the next message, from its sender with its first line,
/// and the `others` with unread messages.
fn wake_text(next: Option<(&str, &str)>, others: &[String]) -> String {
    let Some((sender, first)) = next else {
        return format!("{START}Run `pm msg read` to read your new messages.{END}");
    };
    let mut text = format!("{START}From {sender}");
    if !first.is_empty() {
        text.push_str(&format!(": \"{first}\""));
    }
    text.push_str(". Run `pm msg read` to read it.");
    if !others.is_empty() {
        text.push_str(&format!(" Also unread: {}.", others.join(", ")));
    }
    text.push_str(END);
    text
}

/// Whether `prompt` is one [`wake`] wrote, or an older pm's wake-up.
pub fn is_continuation(prompt: &str) -> bool {
    let prompt = prompt.trim();
    let between = |start: &str, end: &str| {
        prompt
            .strip_prefix(start)
            .and_then(|rest| rest.strip_suffix(end))
            .filter(|middle| !middle.contains('\n'))
    };
    between(START, END).is_some_and(|middle| !middle.is_empty())
        || between(OLD_START, OLD_END).is_some_and(|middle| {
            middle.is_empty() || middle.strip_prefix(" from ").is_some_and(|s| !s.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{self, test_support::messages_dir};

    #[test]
    fn the_wake_names_the_next_sender_and_its_first_line_then_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mdir = messages_dir(dir.path());
        assert!(is_continuation(&wake(&mdir, "login", "impl").unwrap()));

        messages::send(
            &mdir,
            "login",
            "impl",
            "reviewer",
            "\nTwo issues in the parser\nmore",
        )
        .unwrap();
        messages::send(&mdir, "login", "impl", "qa", "all green").unwrap();
        let text = wake(&mdir, "login", "impl").unwrap();
        assert!(
            text.contains("From reviewer@login: \"Two issues in the parser\". Run `pm msg read`"),
            "{text}"
        );
        assert!(text.contains("Also unread: qa@login."), "{text}");
        assert!(!text.contains("more"), "{text}");
        assert!(is_continuation(&text), "{text}");

        messages::next(&mdir, "login", "impl", "reviewer@login").unwrap();
        let text = wake(&mdir, "login", "impl").unwrap();
        assert!(text.contains("From qa@login: \"all green\""), "{text}");
        assert!(!text.contains("Also unread"), "{text}");
    }

    #[test]
    fn a_typed_prompt_is_no_continuation() {
        for prompt in [
            "Run `pm msg read` to read your messages.",
            "[pm message] two\nlines (pm delivers messages this way; this is not an error.)",
            "[pm message]  (pm delivers messages this way; this is not an error.)",
            "You have new messages from . Run `pm msg read` to read them.",
        ] {
            assert!(!is_continuation(prompt), "{prompt}");
        }
    }

    #[test]
    fn an_older_pms_wake_is_still_a_continuation() {
        for prompt in [
            "You have new messages. Run `pm msg read` to read them.",
            "You have new messages from qa@login, lead@login. Run `pm msg read` to read them.",
        ] {
            assert!(is_continuation(prompt), "{prompt}");
        }
    }
}
