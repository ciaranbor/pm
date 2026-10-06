//! Transition alerts and the status-line announcement that shows them.

use chrono::{DateTime, Utc};

use crate::tmux::options::{self, Command, Options, Scope, format_text};

use super::super::attention::{AgentSnapshot, Attention, AttentionKind};
use super::{
    ANNOUNCEMENT, ANNOUNCEMENT_HIDDEN, ANNOUNCEMENT_ID, ANNOUNCEMENT_WINDOW, DISPLAY_TIME,
};

pub(super) struct Alert<'a> {
    text: String,
    /// The pane of the agent asking, whose window's viewers it doesn't need
    /// to reach.
    pane: Option<&'a str>,
}

pub(super) fn alert<'a>(
    session: &str,
    attention: &Attention,
    agents: &'a [AgentSnapshot],
) -> Alert<'a> {
    let what = format!("{session} {}", attention.kind);
    let pane = agents
        .iter()
        .filter(|_| attention.kind == AttentionKind::Asking)
        .find(|a| attention.agent.as_ref() == Some(&a.name))
        .and_then(|a| a.pane.as_deref());
    Alert {
        text: match &attention.detail {
            Some(detail) => format!("{what}: {detail}"),
            None => what,
        },
        pane,
    }
}

/// What a status line shows `alerts` with, for as long as tmux shows a
/// message (`display-time`), as of `now`: the text in [`ANNOUNCEMENT`];
/// on each asking agent's window, [`ANNOUNCEMENT_HIDDEN`] and the text
/// without that ask in [`ANNOUNCEMENT_WINDOW`]; and a server-side timer
/// clearing them unless a newer announcement, with another
/// [`ANNOUNCEMENT_ID`], has replaced them. The pair an earlier
/// announcement set comes off first. No alerts, no change.
pub(super) fn announce(alerts: &[Alert], published: &Options, now: DateTime<Utc>) -> Vec<Command> {
    if alerts.is_empty() {
        return Vec::new();
    }
    let joined = |pane: Option<&str>| -> String {
        let texts: Vec<&str> = alerts
            .iter()
            .filter(|a| pane.is_none() || a.pane != pane)
            .map(|a| a.text.as_str())
            .collect();
        if texts.is_empty() {
            String::new()
        } else {
            format_text(&format!("pm: {}", texts.join(" · ")))
        }
    };
    let id = now.timestamp_micros().to_string();
    let mut writes = Vec::new();
    for held in published
        .windows
        .iter()
        .filter(|w| !w.get(ANNOUNCEMENT_HIDDEN).is_empty())
    {
        for name in [ANNOUNCEMENT_HIDDEN, ANNOUNCEMENT_WINDOW] {
            writes.push(options::set(Scope::Window(&held.target), name, None));
        }
    }
    writes.push(options::set(
        Scope::Global,
        ANNOUNCEMENT,
        Some(&joined(None)),
    ));
    writes.push(options::set(Scope::Global, ANNOUNCEMENT_ID, Some(&id)));
    let mut clear = format!("set -gqu {ANNOUNCEMENT} ; set -gqu {ANNOUNCEMENT_ID}");
    let panes: std::collections::BTreeSet<&str> = alerts.iter().filter_map(|a| a.pane).collect();
    for pane in panes {
        let window = Scope::PaneWindow(pane);
        writes.push(options::set(window, ANNOUNCEMENT_HIDDEN, Some("1")));
        writes.push(options::set(
            window,
            ANNOUNCEMENT_WINDOW,
            Some(&joined(Some(pane))),
        ));
        for name in [ANNOUNCEMENT_HIDDEN, ANNOUNCEMENT_WINDOW] {
            clear.push_str(&format!(" ; set -wqu -t {pane} {name}"));
        }
    }
    // The format is expanded when the timer is set, so the guard is
    // escaped to be read when it fires.
    let guard = format!("if -F \"##{{==:##{{{ANNOUNCEMENT_ID}}},{id}}}\" \"{clear}\"");
    writes.push(options::run_shell_later(
        &display_seconds(published.global.get(DISPLAY_TIME)),
        &guard,
    ));
    writes
}

/// `display_time`, tmux's milliseconds, as seconds. `0`, which keeps a
/// message up until a key is pressed, has no equivalent on the status line
/// and takes tmux's default instead.
fn display_seconds(display_time: &str) -> String {
    let millis = display_time
        .parse::<u32>()
        .ok()
        .filter(|ms| *ms > 0)
        .unwrap_or(750);
    format!("{:.3}", f64::from(millis) / 1000.0)
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use crate::commands::tmux_refresh::{GLOBAL_READ, WINDOW_READ};
    use crate::testing::OwnServer;

    /// What pm's announcement shows on the status line of `window`.
    pub(in crate::commands::tmux_refresh) fn shown(server: &OwnServer, window: &str) -> String {
        server.tmux_stdout(&[
            "display",
            "-p",
            "-t",
            window,
            "#{?@pm_announcement_hidden,#{@pm_announcement_window},#{@pm_announcement}}",
        ])
    }

    pub(in crate::commands::tmux_refresh) fn later(text: &str) -> Alert<'static> {
        Alert {
            text: text.into(),
            pane: None,
        }
    }

    /// Announce `alerts` on `server`, returning what is published after.
    pub(in crate::commands::tmux_refresh) fn publish_announcement(
        server: &OwnServer,
        alerts: &[Alert],
    ) -> Options {
        let read = || {
            options::read(server.name(), &[], WINDOW_READ, GLOBAL_READ)
                .unwrap()
                .unwrap()
        };
        options::run(server.name(), &announce(alerts, &read(), Utc::now())).unwrap();
        read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::OwnServer;
    use crate::tmux;
    use tempfile::tempdir;
    use test_support::*;

    #[test]
    fn an_announcement_is_cleared_after_the_display_time_unless_a_newer_one_replaced_it() {
        let dir = tempdir().unwrap();
        let server = OwnServer::start("announce-expiry");
        tmux::create_session(server.name(), "app/main", dir.path()).unwrap();
        let window = "app/main:0";
        let asking = tmux::new_window(server.name(), "app/main", dir.path(), None, true).unwrap();
        let pane = server.tmux_stdout(&["display", "-p", "-t", &asking, "#{pane_id}"]);
        server.tmux_stdout(&["set", "-g", DISPLAY_TIME, "1000"]);
        let wait = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));

        publish_announcement(&server, &[later("first")]);
        wait(500);
        let ask = Alert {
            text: "asking".into(),
            pane: Some(&pane),
        };
        publish_announcement(&server, &[later("second"), ask]);
        wait(700);
        assert_eq!(
            [shown(&server, window), shown(&server, &asking)],
            ["pm: second · asking", "pm: second"],
            "the first one's timer has fired"
        );
        wait(800);
        assert_eq!([shown(&server, window), shown(&server, &asking)], ["", ""]);
        assert_eq!(
            server.tmux_stdout(&["show", "-wqv", "-t", &asking, ANNOUNCEMENT_HIDDEN]),
            ""
        );
    }
}
