//! Interrupting an agent's turn with the keys its harness reads as an
//! interrupt ([`Harness::interrupt_keys`](crate::harness::Harness::interrupt_keys)),
//! read off its screen, the next skipped once the agent reads as stopped.
//! An agent already at its prompt is sent nothing: there a further Escape
//! opens a harness's own menu (see each harness's interrupt keys).
//!
//! Stopped latches at the first sighting within [`CONFIRM_WITHIN`] of the
//! agent idle, dead, or at its prompt: a waiter with an unread message
//! wakes the agent again at once, so a later look can find it busy on the
//! next turn.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::state::runtime::{Waiting, WaitingClass};
use crate::tmux;

use super::{Liveness, Refusal, find, target};

/// How long an interrupted agent is watched for stopping.
const CONFIRM_WITHIN: Duration = Duration::from_secs(5);
/// The pause between two keys of an interrupt, so the harness reads each
/// as a press of its own: sent together, two keys can arrive as one key
/// sequence.
const KEY_GAP: Duration = Duration::from_millis(120);
const POLL: Duration = Duration::from_millis(100);

/// Interrupt `agent`'s turn; whether it was seen stopped.
pub fn interrupt(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Result<std::result::Result<bool, Refusal>> {
    interrupt_within(
        project_root,
        scope,
        agent,
        tmux_server,
        CONFIRM_WITHIN,
        KEY_GAP,
    )
}

pub(super) fn interrupt_within(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
    within: Duration,
    gap: Duration,
) -> Result<std::result::Result<bool, Refusal>> {
    let target = match target(project_root, scope, agent, tmux_server)? {
        Ok(target) => target,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if stopped(target.liveness, target.at.as_ref()) {
        return Ok(Ok(true));
    }
    let stopped_now = || -> Result<bool> {
        Ok(match find(project_root, scope, agent, tmux_server)? {
            Ok(found) => stopped(found.liveness, found.at.as_ref()),
            Err(_) => true,
        })
    };
    let screen = tmux::capture_screen(tmux_server, &target.pane)?;
    for (i, key) in target.harness.interrupt_keys(&screen).iter().enumerate() {
        if i > 0 {
            std::thread::sleep(gap);
            if stopped_now()? {
                return Ok(Ok(true));
            }
        }
        tmux::send_key(tmux_server, &target.pane, key)?;
    }
    let deadline = Instant::now() + within;
    loop {
        if stopped_now()? {
            return Ok(Ok(true));
        }
        if Instant::now() >= deadline {
            return Ok(Ok(false));
        }
        std::thread::sleep(POLL);
    }
}

/// Whether an agent doing `liveness`, at `at`, runs no turn.
fn stopped(liveness: Liveness, at: Option<&Waiting>) -> bool {
    match liveness {
        Liveness::Idle | Liveness::Dead => true,
        Liveness::Busy => at.is_some_and(|w| w.kind.class() != WaitingClass::Asking),
    }
}
