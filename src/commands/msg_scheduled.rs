//! `pm msg send --in`, `pm msg scheduled` and `pm msg cancel`: a message
//! held until it falls due, then sent through [`send_as`] as if sent then,
//! so it queues, heals and re-arms like any send.
//!
//! No timer process holds it. Every waiter in the recipient's project
//! sweeps the store ([`scheduled`]) as it starts and at each poll
//! of its inbox, delivering every due send of the project, whoever it is
//! for. A send to an idle agent is delivered by its own waiter within a
//! poll; one to a busy agent by the next waiter it starts; one to a dead
//! window by some other waiter of the project, whose send heals it. A
//! project with no waiter running delivers once one starts. Due times are
//! absolute, so a send that fell due while nothing swept goes out at the
//! first sweep after.
//!
//! A recipient checked active when the send was scheduled may be gone when
//! it falls due: that send is kept, marked failed, and never retried.
//! Any other failure leaves it for the next sweep, for ten minutes from the
//! first, after which it is failed too.

use std::path::Path;

use chrono::{DateTime, Local, Utc};

use super::agent_send::{Home, Sender, check_recipient, send_as};
use crate::error::{PmError, Result};
use crate::state::runtime::scheduled::{self, Scheduled, ScheduledHome};

/// Parse a delay such as `90s`, `10m`, `2h`, `1d` or `1h30m`: one or more
/// whole numbers, each followed by its unit. A zero delay is invalid.
pub fn parse_delay(text: &str) -> std::result::Result<chrono::Duration, String> {
    let invalid = || format!("invalid delay '{text}': expected e.g. 30s, 10m, 2h, 1d or 1h30m");
    let mut total = chrono::Duration::zero();
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: i64 = digits.parse().map_err(|_| invalid())?;
        digits.clear();
        let part = match c {
            's' => chrono::Duration::try_seconds(n),
            'm' => chrono::Duration::try_minutes(n),
            'h' => chrono::Duration::try_hours(n),
            'd' => chrono::Duration::try_days(n),
            _ => None,
        };
        total = part
            .and_then(|p| total.checked_add(&p))
            .ok_or_else(invalid)?;
    }
    if !digits.is_empty() || total <= chrono::Duration::zero() {
        return Err(invalid());
    }
    Ok(total)
}

/// Hold `body`, from `sender` to `recipient` in `scope` of the project at
/// `target_root`, until `delay` from now. A recipient no send could reach
/// fails now, as [`send_as`] would.
pub fn schedule(
    target_root: &Path,
    scope: &str,
    recipient: &str,
    sender: Sender<'_>,
    body: &str,
    delay: chrono::Duration,
) -> Result<Scheduled> {
    check_recipient(target_root, scope, recipient, sender)?;
    let now = Utc::now();
    scheduled::schedule(
        target_root,
        Scheduled {
            id: String::new(),
            due: now + delay,
            created: now,
            scope: scope.to_string(),
            recipient: recipient.to_string(),
            sender: sender.name.to_string(),
            home: sender.home.map(|h| ScheduledHome {
                root: h.root.to_path_buf(),
                scope: h.scope.to_string(),
            }),
            body: body.to_string(),
            failed: None,
            retrying_since: None,
        },
    )
}

/// The line `pm msg send --in` prints for `send`.
pub fn describe_scheduled(send: &Scheduled) -> String {
    format!(
        "Message to '{}@{}' scheduled for {} (in {}), id {} — cancel with `pm msg cancel {}`",
        send.recipient,
        send.scope,
        local(send.due),
        delay_text(send.due - send.created),
        send.id,
        send.id
    )
}

/// `delay` as [`parse_delay`] reads it, largest units first.
fn delay_text(delay: chrono::Duration) -> String {
    let mut left = delay.num_seconds();
    let mut text = String::new();
    for (unit, secs) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if left >= secs {
            text.push_str(&format!("{}{unit}", left / secs));
            left %= secs;
        }
    }
    text
}

/// A line for each of the project's scheduled sends, soonest due first.
pub fn list(project_root: &Path) -> Result<Vec<String>> {
    Ok(scheduled::list(project_root)?
        .iter()
        .map(|send| {
            let from = match &send.home {
                Some(home) => format!("{}@{}", send.sender, home.scope),
                None => send.sender.clone(),
            };
            let first = send.body.lines().next().unwrap_or_default();
            let failed = send
                .failed
                .as_deref()
                .map(|why| format!("\n    failed: {why}"))
                .unwrap_or_default();
            format!(
                "{}  {}  {from} → {}@{}  {first}{failed}",
                send.id,
                local(send.due),
                send.recipient,
                send.scope
            )
        })
        .collect())
}

/// Cancel the project's scheduled send whose id starts with `prefix`.
pub fn cancel(project_root: &Path, prefix: &str) -> Result<String> {
    let send = scheduled::remove(project_root, prefix)?;
    Ok(format!(
        "Cancelled scheduled send {} to '{}@{}'",
        send.id, send.recipient, send.scope
    ))
}

fn local(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// How long after its first failed delivery a send is retried, for an
/// error other than a missing recipient, before it is failed for good.
const RETRY_WINDOW: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

/// How a waiter sweeps: the tmux server sends heal through, and what it
/// does once a sweep delivered something.
#[derive(Clone, Copy)]
pub struct Sweeper<'a> {
    pub tmux_server: Option<&'a str>,
    pub on_delivered: &'a dyn Fn(),
}

/// What a sweep did.
#[derive(Debug, Default)]
pub struct Swept {
    /// A line for each send it sent or could not, for the caller's log.
    pub lines: Vec<String>,
    /// Whether any message was queued.
    pub delivered: bool,
}

/// Send every send of the project at `project_root` due by `now`. It never
/// fails, so a waiter that sweeps is never ended by it.
pub fn deliver_due(project_root: &Path, now: DateTime<Utc>, tmux_server: Option<&str>) -> Swept {
    let mut swept = Swept::default();
    let claimed = match scheduled::claim_due(project_root, now) {
        Ok(claimed) => claimed,
        Err(e) => {
            swept
                .lines
                .push(format!("scheduled sends could not be claimed: {e}"));
            return swept;
        }
    };
    for claimed in claimed {
        let send = &claimed.send;
        let what = format!(
            "scheduled send {} to '{}@{}'",
            send.id, send.recipient, send.scope
        );
        let sender = Sender {
            name: &send.sender,
            home: send.home.as_ref().map(|h| Home {
                root: &h.root,
                scope: &h.scope,
            }),
        };
        let sent = send_as(
            project_root,
            &send.scope,
            &send.recipient,
            sender,
            &send.body,
            tmux_server,
        );
        let (line, kept) = match sent {
            Ok(sent) => {
                swept.delivered = true;
                let mut line = format!("{what}: sent");
                if let Some(heal) = sent.heal {
                    line.push_str(&format!("; {}", heal.report));
                }
                for warning in sent.warnings {
                    line.push_str(&format!("; {warning}"));
                }
                (line, scheduled::finish(claimed))
            }
            Err(PmError::AgentNotFound(why)) => (
                format!("{what}: failed: {why}"),
                scheduled::fail(claimed, why),
            ),
            Err(e)
                if send
                    .retrying_since
                    .is_some_and(|since| now - since >= RETRY_WINDOW) =>
            {
                let why = format!(
                    "retried for {}, the last error: {e}",
                    delay_text(RETRY_WINDOW)
                );
                (
                    format!("{what}: failed: {why}"),
                    scheduled::fail(claimed, why),
                )
            }
            Err(e) => (
                format!("{what}: not sent, retried next sweep: {e}"),
                scheduled::retry(claimed, now),
            ),
        };
        swept.lines.push(line);
        if let Err(e) = kept {
            swept
                .lines
                .push(format!("{what}: its record could not be updated: {e}"));
        }
    }
    swept
}

#[cfg(test)]
mod tests;
