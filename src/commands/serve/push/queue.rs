//! What the pusher sends, and when. A transition waits out a grace period
//! before it is pushed, and one whose episode ends meanwhile is never
//! pushed, nor is its end: a need met at the terminal within seconds never
//! reaches the phone. Any other end is pushed at once.

use std::time::{Duration, Instant};

use crate::commands::attention::transition::{Ended, Transition};

/// One push to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outgoing {
    Begun(Transition),
    Ended(Ended),
}

pub(super) struct Queue {
    grace: Duration,
    /// Transitions not yet pushed, each with when it is due, oldest first.
    held: Vec<(Instant, Transition)>,
}

impl Queue {
    pub(super) fn new(grace: Duration) -> Self {
        Self {
            grace,
            held: Vec::new(),
        }
    }

    /// Take what a poll at `now` found: each end cancels the transition it
    /// ends if that is still held, and is otherwise pushed now.
    pub(super) fn take(
        &mut self,
        begun: Vec<Transition>,
        ended: Vec<Ended>,
        now: Instant,
    ) -> Vec<Outgoing> {
        let mut out = Vec::new();
        for end in ended {
            match self.held.iter().position(|(_, t)| end.ends(t)) {
                Some(i) => {
                    self.held.remove(i);
                }
                None => out.push(Outgoing::Ended(end)),
            }
        }
        let due = now + self.grace;
        self.held.extend(begun.into_iter().map(|t| (due, t)));
        out
    }

    /// The held transitions due by `now`.
    pub(super) fn due(&mut self, now: Instant) -> Vec<Outgoing> {
        let n = self.held.partition_point(|(due, _)| *due <= now);
        self.held
            .drain(..n)
            .map(|(_, t)| Outgoing::Begun(t))
            .collect()
    }

    /// Every held transition, due or not.
    pub(super) fn drain(&mut self) -> Vec<Outgoing> {
        self.held
            .drain(..)
            .map(|(_, t)| Outgoing::Begun(t))
            .collect()
    }

    /// When the next held transition is due.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.held.first().map(|(due, _)| *due)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::attention::{Attention, AttentionKind};

    fn asking(agent: &str) -> Transition {
        Transition {
            project: "app".into(),
            scope: "login".into(),
            attention: Attention {
                kind: AttentionKind::Asking,
                detail: Some("Allow Bash?".into()),
                agent: Some(agent.into()),
            },
        }
    }

    fn ended(kind: AttentionKind) -> Ended {
        Ended {
            project: "app".into(),
            scope: "login".into(),
            kind,
            agent: None,
        }
    }

    #[test]
    fn a_need_met_within_the_grace_is_never_pushed_nor_its_end() {
        let grace = Duration::from_secs(10);
        let start = Instant::now();
        let mut queue = Queue::new(grace);
        assert!(queue.take(vec![asking("qa")], Vec::new(), start).is_empty());
        assert_eq!(queue.next_due(), Some(start + grace));

        let later = start + Duration::from_secs(5);
        assert!(
            queue
                .take(Vec::new(), vec![ended(AttentionKind::Asking)], later)
                .is_empty()
        );
        assert!(queue.due(start + grace).is_empty());
        assert_eq!(queue.next_due(), None);
    }

    #[test]
    fn a_need_outlasting_the_grace_is_pushed_then_its_end() {
        let grace = Duration::from_secs(10);
        let start = Instant::now();
        let mut queue = Queue::new(grace);
        queue.take(vec![asking("qa")], Vec::new(), start);
        assert!(queue.due(start + grace / 2).is_empty());
        assert_eq!(queue.due(start + grace), [Outgoing::Begun(asking("qa"))]);

        let end = ended(AttentionKind::Asking);
        assert_eq!(
            queue.take(Vec::new(), vec![end.clone()], start + grace * 2),
            [Outgoing::Ended(end)]
        );
    }

    #[test]
    fn an_end_of_nothing_held_goes_at_once_while_a_new_transition_waits() {
        let grace = Duration::from_secs(10);
        let start = Instant::now();
        let mut queue = Queue::new(grace);
        let end = ended(AttentionKind::Blocked);
        assert_eq!(
            queue.take(vec![asking("qa")], vec![end.clone()], start),
            [Outgoing::Ended(end)]
        );
        assert_eq!(queue.next_due(), Some(start + grace));
    }
}
