use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMeta {
    pub sender: String,
    pub timestamp: DateTime<Utc>,
    /// The scope (feature name or "main") the sender was in when the message
    /// was sent. `None` for messages sent before cross-scope support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_scope: Option<String>,
    /// The project the sender was in when the message was sent.
    /// Only set for cross-project messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_project: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub index: u32,
    pub sender: String,
    pub body: String,
    pub meta: MessageMeta,
}

#[derive(Debug, Clone)]
pub struct UnreadSummary {
    pub sender: String,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    /// Already processed (index <= cursor).
    Read,
    /// The next message to be read (index == cursor + 1).
    Next,
    /// Ahead of the cursor (index > cursor + 1).
    Queued,
}

#[derive(Debug, Clone)]
pub struct MessageSummary {
    pub sender: String,
    pub index: u32,
    pub timestamp: DateTime<Utc>,
    pub first_line: String,
    pub status: MessageStatus,
}

/// Result of resolving which sender a command operates on when `--from`
/// is not specified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderResolution {
    /// `--from` was given explicitly.
    Explicit(String),
    /// Exactly one sender has unread messages; use it.
    Implicit(String),
    /// The inbox has no unread messages at all.
    NoUnread,
    /// More than one sender has unread messages; caller must disambiguate.
    Ambiguous(Vec<String>),
}

impl SenderResolution {
    /// Return the resolved sender, or a messaging error describing the
    /// reason resolution failed.
    pub fn into_sender(self) -> Result<String> {
        match self {
            SenderResolution::Explicit(s) | SenderResolution::Implicit(s) => Ok(s),
            SenderResolution::NoUnread => Err(PmError::Messaging("No new messages".to_string())),
            SenderResolution::Ambiguous(senders) => Err(PmError::Messaging(format!(
                "messages from multiple senders are unread, specify --from {{{}}}",
                senders.join(",")
            ))),
        }
    }
}

/// Cursor tracks the last-read index per sender.
pub(crate) type Cursor = BTreeMap<String, u32>;

/// Metadata about the most recently read message, used by `pm msg reply`
/// to auto-route replies back to the sender's scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastRead {
    pub sender: String,
    pub sender_scope: Option<String>,
    pub sender_project: Option<String>,
    pub index: u32,
}
