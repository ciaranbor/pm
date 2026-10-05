//! An agent's conversation over HTTP: a page of it, one tool result whole,
//! and the watch an event stream keeps on it. The contract is
//! docs/remote-api.md's "Transcript contract".
//!
//! A watch re-reads the registry each poll, so a restart or fork that
//! changes the agent's session is seen: the stream is then sent the new
//! conversation's latest page, flagged `reset`, in place of a tail. So is
//! a conversation that appears after the watch began, which no page could
//! have held. The conversation is located again only when the session
//! changes or its file goes, and a file whose size and mtime are unchanged
//! is not read.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

use crate::error::Result;
use crate::harness::Conversation;
use crate::harness::transcript::items::{Item, Page, Tail, VERSION};
use crate::state::agent::{AgentEntry, AgentRegistry};
use crate::state::paths;

/// Items in a page when the request names no `limit`, and the most it may.
pub(super) const DEFAULT_LIMIT: usize = 50;
pub(super) const MAX_LIMIT: usize = 200;

/// A registered agent, by the names a request gave.
#[derive(Debug, Clone)]
pub(super) struct Agent {
    pub project: String,
    pub scope: String,
    pub name: String,
    pub root: PathBuf,
}

impl Agent {
    /// The agent's registry entry; `None` once it is gone.
    fn entry(&self) -> Result<Option<AgentEntry>> {
        let registry = AgentRegistry::load(&paths::agents_dir(&self.root), &self.scope)?;
        Ok(registry.get(&self.name).cloned())
    }

    /// The harness the agent runs on; `None` once it is gone.
    pub(super) fn harness(&self) -> Result<Option<crate::harness::Harness>> {
        Ok(self.entry()?.map(|entry| entry.harness))
    }

    /// The conversation of the session `entry` records.
    fn locate(&self, entry: &AgentEntry) -> Result<Option<Conversation>> {
        entry.conversation(&self.root, &self.scope, &self.name)
    }

    /// The conversation of the agent's current session; `None` while it
    /// has none, or the agent is gone.
    pub(super) fn conversation(&self) -> Result<Option<Conversation>> {
        match self.entry()? {
            Some(entry) => self.locate(&entry),
            None => Ok(None),
        }
    }
}

#[derive(Serialize)]
struct PageBody<'a> {
    version: u32,
    harness: &'a str,
    #[serde(flatten)]
    page: &'a Page,
}

/// `GET …/transcript`'s body.
pub(super) fn page_json(conversation: &Conversation, page: &Page) -> Result<String> {
    Ok(serde_json::to_string(&PageBody {
        version: VERSION,
        harness: conversation.harness().as_str(),
        page,
    })?)
}

#[derive(Serialize)]
struct Event<'a> {
    project: &'a str,
    scope: &'a str,
    agent: &'a str,
    reset: bool,
    items: &'a [Item],
    after: &'a str,
    /// Only on a reset: where the page sent with it ends.
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<Option<&'a str>>,
}

/// What a stream watching one agent has sent it.
pub(super) struct TranscriptWatch {
    agent: Agent,
    /// The conversation read, and the session id it was found for: found
    /// again only when that id changes or its file goes.
    located: Option<(String, Conversation)>,
    /// Whether a poll found the agent without a conversation, so the one
    /// that appears next is sent whole.
    absent: bool,
    /// Where the next tail starts; `None` to start at the end.
    after: Option<String>,
    /// The conversation file's size and mtime at the last tail.
    stamp: Option<(u64, SystemTime)>,
    /// The JSON last sent for each item of the last tail, so a row read
    /// again unchanged is not sent again.
    sent: HashMap<String, String>,
}

impl TranscriptWatch {
    /// A watch of `agent` from cursor `after`, else from what its
    /// conversation holds now.
    pub(super) fn new(agent: Agent, after: Option<String>) -> Self {
        Self {
            agent,
            located: None,
            absent: false,
            after,
            stamp: None,
            sent: HashMap::new(),
        }
    }

    /// The `transcript` event's data for what changed since the last poll;
    /// `None` when nothing did.
    pub(super) fn poll(&mut self) -> Result<Option<String>> {
        let Some(entry) = self.agent.entry()? else {
            self.vanish();
            return Ok(None);
        };
        let conversation = match &self.located {
            Some((session, known)) if *session == entry.session_id && known.exists() => {
                known.clone()
            }
            _ => match self.agent.locate(&entry)? {
                Some(found) => found,
                None => {
                    self.vanish();
                    return Ok(None);
                }
            },
        };
        let previous = self
            .located
            .replace((entry.session_id, conversation.clone()))
            .map(|(_, c)| c);
        let appeared = previous.is_none() && std::mem::take(&mut self.absent);
        if appeared || previous.is_some_and(|p| p != conversation) {
            return self.reset(&conversation).map(Some);
        }
        let Some(after) = self.after.clone() else {
            let end = conversation.page(None, 0)?.after;
            self.begin(&conversation, end)?;
            return Ok(None);
        };
        let stamp = conversation.stamp();
        if stamp.is_some() && stamp == self.stamp {
            return Ok(None);
        }
        let tail = conversation.tail(&after)?;
        self.stamp = stamp;
        match tail {
            Tail::Reset => self.reset(&conversation).map(Some),
            Tail::Items { items, after } => {
                self.after = Some(after.clone());
                let fresh = self.unsent(items)?;
                if fresh.is_empty() {
                    return Ok(None);
                }
                self.event(false, &fresh, &after, None).map(Some)
            }
        }
    }

    /// The agent has no conversation now.
    fn vanish(&mut self) {
        self.located = None;
        self.absent = true;
    }

    /// Those of `items` not sent as they are by the last tail, which
    /// these replace as the record of what was sent.
    fn unsent(&mut self, items: Vec<Item>) -> Result<Vec<Item>> {
        if items.is_empty() {
            return Ok(items);
        }
        let mut sent = HashMap::new();
        let mut fresh = Vec::new();
        for item in items {
            let json = serde_json::to_string(&item)?;
            if self.sent.get(&item.id) != Some(&json) {
                fresh.push(item.clone());
            }
            sent.insert(item.id, json);
        }
        self.sent = sent;
        Ok(fresh)
    }

    fn reset(&mut self, conversation: &Conversation) -> Result<String> {
        let page = conversation.page(None, DEFAULT_LIMIT)?;
        self.begin(conversation, page.after.clone())?;
        self.event(true, &page.items, &page.after, page.before.as_deref())
    }

    /// Tail from `after` on, the client holding what it holds now: what a
    /// tail from there would read again counts as sent.
    fn begin(&mut self, conversation: &Conversation, after: String) -> Result<()> {
        self.stamp = conversation.stamp();
        self.sent.clear();
        if let Tail::Items { items, .. } = conversation.tail(&after)? {
            self.unsent(items)?;
        }
        self.after = Some(after);
        Ok(())
    }

    fn event(
        &self,
        reset: bool,
        items: &[Item],
        after: &str,
        before: Option<&str>,
    ) -> Result<String> {
        Ok(serde_json::to_string(&Event {
            project: &self.agent.project,
            scope: &self.agent.scope,
            agent: &self.agent.name,
            reset,
            items,
            after,
            before: reset.then_some(before),
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{Harness, opencode_testing};
    use crate::state::agent::AgentType;
    use serde_json::{Value, json};

    fn ids(event: &str) -> Vec<String> {
        let event: Value = serde_json::from_str(event).unwrap();
        event["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_watch_sends_a_reread_row_only_once_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let session = "ses_watch_dedupe";
        let mut registry = AgentRegistry::default();
        registry.register(
            "oc",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: session.to_string(),
                window_name: "oc".to_string(),
                active: true,
                agent_definition: None,
                harness: Harness::OpenCode,
                spawned_at: None,
            },
        );
        registry.save(&paths::agents_dir(&root), "main").unwrap();
        let conn = opencode_testing::create(&opencode_testing::db(&paths::home_dir().unwrap()));
        let insert = |id, seq, updated, text: &str| {
            let data = json!({"time": {"created": updated},
                              "content": [{"type": "text", "text": text}]});
            opencode_testing::insert(&conn, session, id, "assistant", seq, updated, &data);
        };
        insert("m1", 1, 100, "Work");

        let agent = Agent {
            project: "p".into(),
            scope: "main".into(),
            name: "oc".into(),
            root,
        };
        let mut watch = TranscriptWatch::new(agent, None);
        assert_eq!(watch.poll().unwrap(), None, "the watch starts at the end");
        assert_eq!(
            watch.poll().unwrap(),
            None,
            "the row at the cursor's millisecond is read again, unchanged"
        );

        insert("m1", 1, 100, "Work done");
        let event = watch.poll().unwrap().expect("the rewritten row");
        assert_eq!(ids(&event), ["m1:0"]);
        assert!(event.contains("Work done"));

        insert("m2", 2, 101, "Next");
        let event = watch.poll().unwrap().expect("the new row");
        assert_eq!(ids(&event), ["m2:0"]);
        assert_eq!(watch.poll().unwrap(), None);
    }
}
