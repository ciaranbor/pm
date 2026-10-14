//! The waiter's watch on its agent's screen, for a harness that draws
//! dialogs between turns that no hook reports, or that none reports
//! closing ([`Harness::unhooked_dialog_window`]): it marks the agent asking
//! while one shows, and puts the waiter's marker back once one it saw has
//! gone.
//!
//! It reads the screen on every poll for the harness's window after the
//! waiter starts, and after that only while a dialog marker held between
//! turns waits to be seen closing. It replaces only a marker it read, so
//! one a hook wrote meanwhile stands, and only idle or a dialog: a
//! permission, question, plan or background marker is a hook's or the
//! waiter's own. It clears a dialog marker only once the screen has shown
//! a dialog, so one a notification reports that the screen reader does not
//! recognise stays asking.

use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::commands::attention::AgentState;
use crate::commands::hooks_waiting;
use crate::commands::running_agents::AgentAt;
use crate::error::Result;
use crate::state::runtime::{self, Waiting, WaitingKind};

/// A waiter's watch on its agent's screen.
pub(super) struct Watch<'a, C> {
    at: AgentAt<'a>,
    capture: C,
    /// Until when the screen is read with no dialog marker held; `None`
    /// for a harness whose screen it never reads.
    until: Option<Instant>,
    idle_since: DateTime<Utc>,
    /// The detail of the dialog the woken turn ran through, for one the
    /// screen still shows without a title.
    carried: Option<String>,
    seen: bool,
    marked: Option<Waiting>,
}

impl<'a, C: FnMut() -> Option<String>> Watch<'a, C> {
    /// A watch whose `capture` returns the agent's screen with its escape
    /// sequences.
    pub(super) fn new(
        at: AgentAt<'a>,
        carried: Option<String>,
        idle_since: DateTime<Utc>,
        until: Option<Instant>,
        capture: C,
    ) -> Self {
        Self {
            at,
            capture,
            until,
            idle_since,
            carried,
            seen: false,
            marked: None,
        }
    }

    /// The dialog marker this watch last wrote, if any.
    pub(super) fn marked(&self) -> Option<&Waiting> {
        self.marked.as_ref()
    }

    /// Read the screen when due and re-mark the agent by it. Returns the
    /// agent's state when its marker changed.
    pub(super) fn check(&mut self) -> Result<Option<AgentState>> {
        let AgentAt {
            harness,
            project_root,
            scope,
            name: agent,
        } = self.at;
        let Some(until) = self.until else {
            return Ok(None);
        };
        let Some(held) = runtime::read_waiting(project_root, scope, agent) else {
            return Ok(None);
        };
        let holding =
            held.kind == WaitingKind::Dialog && held.between_turns && held.subagent.is_none();
        if !holding && Instant::now() > until {
            return Ok(None);
        }
        let Some(screen) = (self.capture)() else {
            return Ok(None);
        };
        let shown = harness.screen_dialog(&screen);
        self.seen |= shown.is_some();
        let new = match shown {
            Some(shown) if held.kind == WaitingKind::Idle => Waiting {
                detail: shown.detail.or_else(|| self.carried.clone()),
                ..shown
            },
            None if holding && self.seen => {
                self.seen = false;
                hooks_waiting::between_turns(project_root, scope, agent, harness, self.idle_since)
            }
            _ => return Ok(None),
        };
        if !runtime::replace_waiting_if(project_root, scope, agent, &held, &new)? {
            return Ok(None);
        }
        let state = hooks_waiting::state_of(&new);
        self.marked = (new.kind == WaitingKind::Dialog).then_some(new);
        Ok(Some(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use std::cell::RefCell;
    use tempfile::tempdir;

    fn fixture(name: &str) -> String {
        crate::testing::screen_fixture(&format!("claude-code-{name}"))
    }

    fn at(root: &std::path::Path) -> AgentAt<'_> {
        AgentAt {
            harness: Harness::ClaudeCode,
            project_root: root,
            scope: "login",
            name: "implementer",
        }
    }

    fn marker(root: &std::path::Path) -> Option<Waiting> {
        runtime::read_waiting(root, "login", "implementer")
    }

    fn open() -> Option<Instant> {
        Some(Instant::now() + std::time::Duration::from_secs(60))
    }

    fn idle() -> Waiting {
        Waiting {
            between_turns: true,
            ..Waiting::now(WaitingKind::Idle, None)
        }
    }

    #[test]
    fn a_dialog_drawn_as_the_turn_ends_asks_until_it_is_gone() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        runtime::write_waiting(root, "login", "implementer", &idle()).unwrap();
        let screen = RefCell::new(fixture("teach-below"));
        let mut watch = Watch::new(at(root), None, Utc::now(), open(), || {
            Some(screen.borrow().clone())
        });

        assert_eq!(watch.check().unwrap(), Some(AgentState::Asking));
        let asking = marker(root).unwrap();
        assert_eq!(asking.kind, WaitingKind::Dialog);
        assert!(asking.between_turns);
        assert_eq!(asking.describe(), "Teach auto mode about your environment?");
        assert_eq!(watch.check().unwrap(), None, "still drawn");

        *screen.borrow_mut() = fixture("scanning");
        assert_eq!(watch.check().unwrap(), Some(AgentState::Idle));
        assert_eq!(marker(root).unwrap().kind, WaitingKind::Idle);
        assert_eq!(watch.check().unwrap(), None);
    }

    #[test]
    fn once_its_window_has_passed_an_idle_agents_screen_is_not_read() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        runtime::write_waiting(root, "login", "implementer", &idle()).unwrap();
        let past = Instant::now() - std::time::Duration::from_secs(1);
        let mut watch = Watch::new(at(root), None, Utc::now(), Some(past), || {
            panic!("read the screen")
        });
        assert_eq!(watch.check().unwrap(), None);
        assert_eq!(marker(root).unwrap().kind, WaitingKind::Idle);
    }

    #[test]
    fn a_hooks_marker_is_never_replaced() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let permission = Waiting {
            between_turns: true,
            ..Waiting::now(WaitingKind::Permission, Some("Bash: ls".into()))
        };
        runtime::write_waiting(root, "login", "implementer", &permission).unwrap();
        let mut watch = Watch::new(at(root), None, Utc::now(), open(), || {
            Some(fixture("teach-below"))
        });
        assert_eq!(watch.check().unwrap(), None);
        assert_eq!(marker(root), Some(permission));
    }

    #[test]
    fn a_notified_dialog_is_cleared_only_once_the_screen_showed_one() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let notified = Waiting {
            between_turns: true,
            ..Waiting::now(WaitingKind::Dialog, Some("Claude needs your input".into()))
        };
        runtime::write_waiting(root, "login", "implementer", &notified).unwrap();
        let screen = RefCell::new(fixture("busy"));
        let mut watch = Watch::new(at(root), None, Utc::now(), open(), || {
            Some(screen.borrow().clone())
        });

        assert_eq!(watch.check().unwrap(), None, "a dialog it can't read");
        assert_eq!(marker(root), Some(notified));

        *screen.borrow_mut() = fixture("proposal");
        assert_eq!(watch.check().unwrap(), None);
        *screen.borrow_mut() = fixture("scanning");
        assert_eq!(
            watch.check().unwrap(),
            Some(AgentState::Idle),
            "the proposal's scan aborted"
        );
    }
}
