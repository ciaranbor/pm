//! Whether the macOS login keychain answers. A harness that reads it as it
//! starts ([`Harness::reads_keychain`]) is held by a keychain daemon that
//! does not answer before its session starts, drawing nothing. Asking it
//! once, bounded, tells pm so before it restarts agents into that.
//!
//! The question is [`COMMAND`], which reads the keychain's settings and
//! never unlocks it, so it cannot raise a prompt. Off macOS there is no
//! keychain to ask.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::bounded::{self, Failure};
use crate::harness::Harness;

/// How long the keychain may take to answer.
pub const LIMIT: Duration = Duration::from_secs(5);

/// The command pm asks the keychain with, as the user would run it.
pub const COMMAND: &str = "security show-keychain-info login.keychain";

/// How the keychain answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Answered,
    /// It answered with an error, as a locked keychain does over ssh
    /// ("User interaction is not allowed"): what `security` printed.
    Error(String),
    /// It did not answer within [`LIMIT`].
    Hung,
}

/// Ask the login keychain; `None` off macOS, or when `security` cannot be
/// run.
pub fn check() -> Option<Answer> {
    if cfg!(target_os = "macos") {
        ask("security", LIMIT)
    } else {
        None
    }
}

/// The harnesses of `harnesses` that read the keychain, as a phrase:
/// `claude-code and codex`; empty when none does.
pub fn readers(harnesses: impl IntoIterator<Item = Harness>) -> String {
    let mut names: Vec<String> = Vec::new();
    for harness in harnesses.into_iter().filter(|h| h.reads_keychain()) {
        if !names.contains(&harness.to_string()) {
            names.push(harness.to_string());
        }
    }
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

fn ask(program: &str, limit: Duration) -> Option<Answer> {
    let mut command = Command::new(program);
    command
        .args(["show-keychain-info", "login.keychain"])
        .stdin(Stdio::null());
    match bounded::run(&mut command, limit) {
        Ok(out) if out.status.success() => Some(Answer::Answered),
        Ok(out) => {
            let said = String::from_utf8_lossy(&out.stderr);
            let said = said.trim().trim_start_matches("security: ").trim();
            Some(Answer::Error(if said.is_empty() {
                format!("`{COMMAND}` exited with {}", out.status)
            } else {
                said.to_string()
            }))
        }
        Err(Failure::TimedOut { .. }) => Some(Answer::Hung),
        Err(Failure::Unrunnable { .. }) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_security(dir: &std::path::Path, body: &str) -> String {
        let path = dir.join("security");
        crate::testing::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        path.display().to_string()
    }

    #[test]
    fn a_keychain_that_does_not_answer_in_time_is_hung() {
        let dir = tempfile::tempdir().unwrap();
        let security = fake_security(dir.path(), "exec sleep 30");
        assert_eq!(
            ask(&security, Duration::from_millis(300)),
            Some(Answer::Hung)
        );
    }

    #[test]
    fn a_locked_keychain_answers_with_its_error_and_a_working_one_answers() {
        let dir = tempfile::tempdir().unwrap();
        let locked = fake_security(
            dir.path(),
            "echo 'security: SecKeychainCopySettings: User interaction is not allowed.' >&2\n\
             exit 36",
        );
        assert_eq!(
            ask(&locked, LIMIT),
            Some(Answer::Error(
                "SecKeychainCopySettings: User interaction is not allowed.".into()
            ))
        );
        let open = fake_security(dir.path(), "echo 'Keychain \"login\" no-timeout' >&2");
        assert_eq!(ask(&open, LIMIT), Some(Answer::Answered));
        assert_eq!(ask("/nonexistent/security", LIMIT), None);
    }
}
