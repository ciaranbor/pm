//! A queue's key: the address of its sender as the recipient's inbox sees
//! it, `name@scope`, with `@project` appended when the sender is in another
//! project. Senders with no home — pm itself (`no-reply-*`), someone at a
//! shell, or someone outside every project — are keyed by bare name, as is
//! a queue whose messages predate metadata.
//!
//! `--from` takes a whole address, or any leading part of one ending at an
//! `@` (`implementer`, `implementer@login`), as long as it names one queue.

use super::types::MessageMeta;
use super::validation::validate_name;
use crate::error::{PmError, Result};

/// The key of the queue a message carrying `meta` goes to in an inbox of
/// `inbox_scope`.
pub fn queue_key(meta: &MessageMeta, inbox_scope: &str) -> String {
    if meta.from_outside || meta.from_shell || super::is_no_reply(&meta.sender) {
        return meta.sender.clone();
    }
    let scope = meta.sender_scope.as_deref().unwrap_or(inbox_scope);
    match &meta.sender_project {
        Some(project) => format!("{}@{scope}@{project}", meta.sender),
        None => format!("{}@{scope}", meta.sender),
    }
}

/// The sender's name: an address up to its first `@`.
pub fn name_of(key: &str) -> &str {
    key.split('@').next().unwrap_or(key)
}

/// Whether `from` names the queue `key`: the whole address, or a leading
/// part of it that ends at an `@`.
pub fn matches(key: &str, from: &str) -> bool {
    key.strip_prefix(from)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('@'))
}

/// The one key of `keys` that `from` names: an exact match, else the only
/// key it matches. `from` itself when it matches none (a sender yet to
/// send), and an error naming the candidates when it matches several.
pub fn resolve<'a>(keys: impl IntoIterator<Item = &'a str>, from: &str) -> Result<String> {
    validate_key(from)?;
    let candidates: Vec<&str> = keys.into_iter().filter(|k| matches(k, from)).collect();
    if candidates.contains(&from) {
        return Ok(from.to_string());
    }
    match candidates.as_slice() {
        [] => Ok(from.to_string()),
        [one] => Ok(one.to_string()),
        many => Err(PmError::Messaging(format!(
            "'{from}' names more than one sender: {} — pass one of them to --from",
            many.join(", ")
        ))),
    }
}

/// Validate a queue key, every part of which becomes part of a path: a
/// name, then optionally a scope and a project, separated by `@`.
pub fn validate_key(key: &str) -> Result<()> {
    let mut parts = key.split('@');
    validate_name(parts.next().unwrap_or_default(), "sender")?;
    if let Some(scope) = parts.next() {
        validate_name(scope, "sender scope")?;
    }
    if let Some(project) = parts.next() {
        validate_project(project)?;
    }
    if parts.next().is_some() {
        return Err(PmError::InvalidAgentName(format!(
            "sender '{key}' has more than three '@'-separated parts"
        )));
    }
    Ok(())
}

/// A project name is a directory's name, so it may hold dots, but never a
/// separator or a dot-only name.
fn validate_project(project: &str) -> Result<()> {
    if project.is_empty()
        || project.chars().all(|c| c == '.')
        || project.contains(['/', '\\', '\0'])
    {
        return Err(PmError::InvalidAgentName(format!(
            "sender project '{project}' is not a valid project name"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn meta(sender: &str, scope: Option<&str>, project: Option<&str>) -> MessageMeta {
        MessageMeta {
            sender: sender.to_string(),
            timestamp: Utc::now(),
            sender_scope: scope.map(str::to_string),
            sender_project: project.map(str::to_string),
            from_outside: false,
            from_shell: false,
        }
    }

    #[test]
    fn senders_with_a_home_are_keyed_by_scope_and_foreign_project() {
        assert_eq!(
            queue_key(&meta("implementer", None, None), "main"),
            "implementer@main"
        );
        assert_eq!(
            queue_key(&meta("implementer", Some("login"), None), "main"),
            "implementer@login"
        );
        assert_eq!(
            queue_key(&meta("reviewer", Some("main"), Some("exo")), "main"),
            "reviewer@main@exo"
        );
    }

    #[test]
    fn senders_without_a_home_are_keyed_by_name() {
        assert_eq!(
            queue_key(&meta("no-reply-brief", Some("login"), None), "login"),
            "no-reply-brief"
        );
        let outside = MessageMeta {
            from_outside: true,
            ..meta("ciaran", None, None)
        };
        assert_eq!(queue_key(&outside, "main"), "ciaran");
    }

    #[test]
    fn from_resolves_a_unique_prefix_and_refuses_an_ambiguous_one() {
        let keys = [
            "implementer@login",
            "implementer@signup",
            "reviewer@main@exo",
        ];
        assert_eq!(resolve(keys, "reviewer").unwrap(), "reviewer@main@exo");
        assert_eq!(resolve(keys, "reviewer@main").unwrap(), "reviewer@main@exo");
        assert_eq!(
            resolve(keys, "implementer@login").unwrap(),
            "implementer@login"
        );
        let err = resolve(keys, "implementer").unwrap_err().to_string();
        assert!(
            err.contains("implementer@login, implementer@signup"),
            "{err}"
        );
        assert_eq!(resolve(keys, "impl").unwrap(), "impl");
    }

    #[test]
    fn an_exact_key_wins_over_longer_ones() {
        let keys = ["ciaran", "ciaran@main"];
        assert_eq!(resolve(keys, "ciaran").unwrap(), "ciaran");
    }

    #[test]
    fn keys_reject_path_tricks() {
        assert!(validate_key("a@b@my.project").is_ok());
        for bad in ["a/b", "a@..", "a@b@..", "a@b@x/y", "a@b@c@d", "a@", ""] {
            assert!(validate_key(bad).is_err(), "{bad}");
        }
    }
}
