//! Agent messaging: per-agent inboxes on disk, scoped to a feature (or
//! `main`), with one ordered queue and one read cursor per sender, keyed by
//! the sender's address ([`address`]). Wherever this module's API names a
//! `sender`, it means that address; a message's own `meta.sender` is the
//! bare name.
//!
//! Sending appends to the sender's queue; reading by index or listing never
//! moves the cursor, and only [`next`] advances it.

pub mod address;
mod continuation;
mod cursor;
mod delete;
mod last_read;
mod layout;
mod list;
mod read;
mod rekey;
mod resolve;
mod send;
mod types;
mod validation;

pub use continuation::{is_continuation, wake};
pub use cursor::{cursor_for, next};
pub use delete::{delete_feature, delete_inbox};
pub use last_read::{load_last_read, save_last_read};
pub use list::list;
pub use read::{check, last_sent, read_at, read_count, unread_count};
pub use rekey::{rekey, rekey_pending};
pub use resolve::{resolve_from, resolve_sender};
pub use send::{send, send_by, send_full, send_meta, send_with_scope};
pub use types::{
    LastRead, Message, MessageMeta, MessageStatus, MessageSummary, SenderChoice, UnreadSummary,
};
pub use validation::validate_name;

/// Whether `sender` is one pm itself sends as (a feature brief, a forced
/// restart's resume), which no agent answers to.
pub fn is_no_reply(sender: &str) -> bool {
    sender.starts_with("no-reply-")
}

/// Whether no agent runs this process: `PM_AGENT_NAME` (set by
/// `pm agent spawn`) unset or empty, so a send from it has no reply.
pub fn from_shell() -> bool {
    std::env::var("PM_AGENT_NAME").map_or(true, |name| name.is_empty())
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
