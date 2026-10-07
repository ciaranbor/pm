//! Agent messaging: per-agent inboxes on disk, scoped to a feature (or
//! `main`), with one ordered queue and one read cursor per sender.
//!
//! Sending appends to the sender's queue; reading by index or listing never
//! moves the cursor, and only [`next`] advances it.

mod cursor;
mod delete;
mod last_read;
mod layout;
mod list;
mod read;
mod resolve;
mod send;
mod types;
mod validation;

pub use cursor::{cursor_for, next};
pub use delete::{delete_feature, delete_inbox};
pub use last_read::{load_last_read, save_last_read};
pub use list::list;
pub use read::{check, read_at, read_count, unread_count};
pub use resolve::resolve_sender;
pub use send::{send, send_full, send_with_scope};
pub use types::{
    LastRead, Message, MessageMeta, MessageStatus, MessageSummary, SenderChoice, UnreadSummary,
};
pub use validation::validate_name;

/// Format a sender for display, annotating it with the scope (and project) it
/// was sent from when the message carries cross-scope metadata. Mirrors the
/// sender line shown by `pm msg read`, so `pm msg list` and `pm msg read`
/// render senders consistently. Same-scope senders (no recorded scope) render
/// bare.
pub fn format_sender_display(
    sender: &str,
    sender_scope: Option<&str>,
    sender_project: Option<&str>,
) -> String {
    let mut display = match sender_scope {
        Some(scope) => format!("{sender}@{scope}"),
        None => sender.to_string(),
    };
    if let Some(project) = sender_project {
        display = format!("{display} ({project})");
    }
    display
}

/// Whether `sender` is one pm itself sends as (a feature brief, a forced
/// restart's resume), which no agent answers to.
pub fn is_no_reply(sender: &str) -> bool {
    sender.starts_with("no-reply-")
}

/// Default identity: PM_AGENT_NAME (set by `pm agent spawn`) > $USER > "user".
pub fn default_user_name() -> String {
    std::env::var("PM_AGENT_NAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_string())
}

/// Helpers the submodules' tests share.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    pub(crate) fn messages_dir(dir: &Path) -> PathBuf {
        dir.join("messages")
    }
}
