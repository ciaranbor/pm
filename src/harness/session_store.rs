//! What the session seams read and write: a harness's session store, an
//! agent's conversation, and the outcome of an import.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::state::project::HarnessConfig;

use super::transcript::items::{Body, Page, Tail};
use super::transcript::jsonl::{self, Parse};
use super::{Harness, claude_code, opencode};

/// How the session seams reach a harness's store.
#[derive(Debug, Clone, Copy)]
pub struct SessionStore<'a> {
    /// The user's home; a harness with a file store derives it from here.
    pub home: &'a Path,
    /// The `[harness.*]` settings in effect where the sessions belong.
    pub config: &'a HarnessConfig,
}

impl SessionStore<'_> {
    pub(super) fn claude_base(&self) -> PathBuf {
        self.home.join(claude_code::CONFIG_DIR)
    }
}

/// One directory whose sessions [`Harness::export_sessions`] writes.
pub struct ExportJob<'a> {
    /// The `[harness.*]` settings in effect where the sessions belong.
    pub config: &'a HarnessConfig,
    /// The directory the sessions were recorded at.
    pub dir: &'a Path,
    /// Where they are written.
    pub staging: PathBuf,
}

/// The agent whose conversation [`Harness::conversation`] locates.
#[derive(Debug, Clone, Copy)]
pub struct AgentSession<'a> {
    pub project_root: &'a Path,
    pub scope: &'a str,
    pub name: &'a str,
    /// The registry's session id.
    pub session_id: &'a str,
    /// Where the agent runs.
    pub worktree: &'a Path,
    pub home: &'a Path,
}

/// An agent session's conversation, read as the chat view of
/// `transcript::items`. Cursors are opaque to callers: a byte offset for
/// a transcript file; for opencode a `seq` going back and a
/// `time_updated` going forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub(super) harness: Harness,
    pub(super) location: Location,
}

#[derive(Debug, Clone)]
pub(super) enum Location {
    Jsonl { path: PathBuf, parse: Parse },
    Session { db: PathBuf, id: String },
}

/// The same conversation: a harness reads a given file with one parser.
impl PartialEq for Location {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Jsonl { path: a, .. }, Self::Jsonl { path: b, .. }) => a == b,
            (Self::Session { db: a, id: x }, Self::Session { db: b, id: y }) => a == b && x == y,
            _ => false,
        }
    }
}

impl Eq for Location {}

fn unreadable(e: std::io::Error) -> PmError {
    PmError::Transcript(e.to_string())
}

impl Conversation {
    pub fn harness(&self) -> Harness {
        self.harness
    }

    /// Up to about `limit` items before cursor `before` (the end when
    /// `None`, or not a cursor of this conversation), oldest first.
    pub fn page(&self, before: Option<&str>, limit: usize) -> Result<Page> {
        match &self.location {
            Location::Jsonl { path, parse } => {
                let before = before.and_then(|b| b.parse().ok());
                jsonl::page(path, before, limit, *parse).map_err(unreadable)
            }
            Location::Session { db, id } => {
                let before = before.and_then(|b| b.parse().ok());
                opencode::chat::page(db, id, before, limit)
            }
        }
    }

    /// What the conversation gained after cursor `after`.
    pub fn tail(&self, after: &str) -> Result<Tail> {
        match &self.location {
            Location::Jsonl { path, parse } => match after.parse() {
                Ok(after) => jsonl::tail(path, after, *parse).map_err(unreadable),
                Err(_) => Ok(Tail::Reset),
            },
            Location::Session { db, id } => match after.parse() {
                Ok(after) => opencode::chat::tail(db, id, after),
                Err(_) => Ok(Tail::Reset),
            },
        }
    }

    /// Whether the conversation took in a prompt whose text, as typed,
    /// `is_text` after cursor `after`: any prompt, whoever sent it, where the
    /// harness records that; else the user's.
    pub fn took_in(&self, after: &str, is_text: impl Fn(&str) -> bool) -> Result<bool> {
        match (self.harness, &self.location) {
            (Harness::ClaudeCode, Location::Jsonl { path, .. }) => {
                claude_code::chat::took_in(path, after, is_text).map_err(unreadable)
            }
            _ => Ok(match self.tail(after)? {
                Tail::Items { items, .. } => items
                    .iter()
                    .any(|item| matches!(&item.body, Body::User { text } if is_text(text))),
                Tail::Reset => false,
            }),
        }
    }

    /// The whole output of the tool result whose `full` is `reference`.
    pub fn full_result(&self, reference: &str) -> Result<Option<String>> {
        match &self.location {
            Location::Jsonl { path, parse } => {
                jsonl::full_result(path, reference, *parse).map_err(unreadable)
            }
            Location::Session { db, id } => opencode::chat::full_result(db, id, reference),
        }
    }

    /// The size and mtime of the file the conversation is read from; a
    /// tail of an unchanged one finds nothing. `None` when it can't be
    /// told: the file is gone, or the conversation is in a database.
    pub fn stamp(&self) -> Option<(u64, std::time::SystemTime)> {
        match &self.location {
            Location::Jsonl { path, .. } => {
                let meta = std::fs::metadata(path).ok()?;
                Some((meta.len(), meta.modified().ok()?))
            }
            Location::Session { .. } => None,
        }
    }

    /// Whether what the conversation is read from is still there.
    pub fn exists(&self) -> bool {
        match &self.location {
            Location::Jsonl { path, .. } => path.is_file(),
            Location::Session { db, .. } => db.is_file(),
        }
    }
}

/// A session an agent is running on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InUse {
    pub session_id: String,
    /// `<scope>/<agent>`, for the report.
    pub agent: String,
}

/// What importing one directory's sessions did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Nothing was written, and why.
    Skipped(String),
    Imported {
        /// What was imported, for the report.
        detail: String,
        notes: Vec<String>,
    },
}

/// `imported` of `total` sessions were new to the store; the rest were
/// already there and left untouched.
pub(crate) fn per_session_outcome(imported: usize, total: usize) -> ImportOutcome {
    match session_counts(imported, total) {
        Ok(detail) => ImportOutcome::Imported {
            detail,
            notes: Vec::new(),
        },
        Err(why) => ImportOutcome::Skipped(why),
    }
}

/// [`per_session_outcome`]'s wording: the detail when any session was new,
/// else why there was nothing to import.
pub(crate) fn session_counts(imported: usize, total: usize) -> std::result::Result<String, String> {
    if total == 0 {
        return Err("no sessions in the export".to_string());
    }
    if imported == 0 {
        return Err(format!("all {total} session(s) already exist locally"));
    }
    Ok(match total - imported {
        0 => format!("{imported} session(s)"),
        present => format!("{imported} session(s), {present} already present"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::{self, SessionPath};

    #[test]
    fn a_conversation_is_the_recorded_transcript_else_the_one_its_id_names() {
        let dir = tempfile::tempdir().unwrap();
        let (root, home) = (dir.path().join("proj"), dir.path().join("home"));
        let worktree = root.join("main");
        let agent = AgentSession {
            project_root: &root,
            scope: "main",
            name: "impl",
            session_id: "s1",
            worktree: &worktree,
            home: &home,
        };
        assert_eq!(Harness::ClaudeCode.conversation(&agent), None);

        let derived = home
            .join(".claude/projects")
            .join(claude_code::sessions::path_to_key(&worktree))
            .join("s1.jsonl");
        std::fs::create_dir_all(derived.parent().unwrap()).unwrap();
        std::fs::write(&derived, "").unwrap();
        let file = |c: Option<Conversation>| match c.map(|c| c.location) {
            Some(Location::Jsonl { path, .. }) => path,
            other => panic!("{other:?}"),
        };
        assert_eq!(file(Harness::ClaudeCode.conversation(&agent)), derived);

        let recorded = dir.path().join("s2/s1.jsonl");
        std::fs::create_dir_all(recorded.parent().unwrap()).unwrap();
        std::fs::write(&recorded, "").unwrap();
        runtime::write_session_path(
            &root,
            "main",
            "impl",
            SessionPath::Transcript,
            Some(&recorded),
        )
        .unwrap();
        assert_eq!(file(Harness::ClaudeCode.conversation(&agent)), recorded);

        let restarted = AgentSession {
            session_id: "s2",
            ..agent
        };
        assert_eq!(
            Harness::ClaudeCode.conversation(&restarted),
            None,
            "a recorded transcript of another session is not this one's"
        );

        let rollout =
            home.join(".codex/sessions/2026/10/03/rollout-2026-10-03T00-00-00-s1.jsonl.zst");
        std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        std::fs::write(&rollout, "").unwrap();
        let codex = AgentSession {
            name: "cx",
            ..agent
        };
        assert_eq!(file(Harness::Codex.conversation(&codex)), rollout);
    }
}
