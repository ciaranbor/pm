//! Reading and setting an agent's permission mode, in its harness's own
//! vocabulary ([`Harness::modes`]).
//!
//! A harness whose mode moves round a cycle on a key ([`ModeSwitch::Cycle`])
//! is pressed one step at a time, the mode read back off its screen after
//! each press until it changes. Before each press the agent's waiting
//! record and its screen are read again: a dialog takes the key, so a
//! dialog recorded, or a screen without its mode, stops the walk with
//! nothing pressed. A step is judged by the mode having changed, not by
//! one step of the cycle, so a press at the keyboard meanwhile is
//! harmless. A walk back to where it started, or one over [`MAX_PRESSES`],
//! means the mode isn't in this session's cycle; the session is then left
//! where the walk ended, which after a full cycle is where it started.
//!
//! A walk passes through the modes between, each for as long as a press
//! takes to show, mid-turn too: a tool call the agent makes meanwhile runs
//! under that mode's policy, auto-approval included, as it would under the
//! same presses at the keyboard.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::harness::{Harness, ModeSwitch};
use crate::state::runtime::{self, Waiting, WaitingClass};
use crate::tmux;

use super::{Liveness, Refusal, Target, find, target};

/// How long a press is given to change the mode on screen.
const CHANGE_WITHIN: Duration = Duration::from_millis(1500);
/// The most presses one walk makes: more than any cycle's length.
const MAX_PRESSES: usize = 8;
const POLL: Duration = Duration::from_millis(50);

/// `agent`'s mode now, running on `harness`; `None` when it can't be read:
/// it isn't running, or neither its screen nor its session shows it.
pub fn current_mode(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    tmux_server: Option<&str>,
) -> Result<Option<String>> {
    let Some(switch) = harness.mode_switch() else {
        return Ok(None);
    };
    let found = match find(project_root, scope, agent, tmux_server)? {
        Ok(found) if found.runs_harness && found.liveness != Liveness::Dead => found,
        _ => return Ok(None),
    };
    match switch {
        ModeSwitch::Cycle(_) => {
            let screen = tmux::capture_screen(tmux_server, &found.pane.id)?;
            Ok(harness.mode_on_screen(&screen).map(str::to_string))
        }
        ModeSwitch::Server => {
            let Some(inbound) = runtime::read_inbound(project_root, scope, agent) else {
                return Ok(None);
            };
            // A server that can't be reached shows no mode, like a screen
            // without one.
            Ok(harness.server_mode(&inbound).ok().flatten())
        }
    }
}

/// Why `mode` can't be asked of an agent on `harness`, in words naming the
/// modes it can; `None` when it is one of [`Harness::modes`].
pub fn unknown_mode(harness: Harness, mode: &str) -> Option<String> {
    let modes = harness.modes();
    if modes.iter().any(|(id, _)| *id == mode) {
        return None;
    }
    let ids: Vec<&str> = modes.iter().map(|(id, _)| *id).collect();
    Some(format!(
        "{mode} is not one of this agent's modes: {}",
        ids.join(", ")
    ))
}

/// Put `agent` in `mode`; the mode it was then seen in, which is `mode`. A
/// mode [`unknown_mode`] names is [`Refusal::Unavailable`].
pub fn set_mode(
    project_root: &Path,
    scope: &str,
    agent: &str,
    mode: &str,
    tmux_server: Option<&str>,
) -> Result<std::result::Result<String, Refusal>> {
    let at = At {
        project_root,
        scope,
        agent,
        tmux_server,
    };
    set_mode_within(&at, mode, CHANGE_WITHIN)
}

/// The agent a walk is for.
pub(super) struct At<'a> {
    pub project_root: &'a Path,
    pub scope: &'a str,
    pub agent: &'a str,
    pub tmux_server: Option<&'a str>,
}

pub(super) fn set_mode_within(
    at: &At<'_>,
    mode: &str,
    change_within: Duration,
) -> Result<std::result::Result<String, Refusal>> {
    let target = match target(at.project_root, at.scope, at.agent, at.tmux_server)? {
        Ok(target) => target,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if unknown_mode(target.harness, mode).is_some() {
        return Ok(Err(Refusal::Unavailable));
    }
    match target.harness.mode_switch() {
        None => Ok(Err(Refusal::Unavailable)),
        Some(ModeSwitch::Cycle(key)) => cycle(at, &target, key, mode, change_within),
        Some(ModeSwitch::Server) => {
            let Some(inbound) = runtime::read_inbound(at.project_root, at.scope, at.agent) else {
                return Ok(Err(Refusal::NotRunning));
            };
            Ok(match target.harness.set_mode(&inbound, mode)? {
                Some(now) if now == mode => Ok(now),
                _ => Err(Refusal::Unchanged),
            })
        }
    }
}

fn asking(at: Option<&Waiting>) -> Option<Refusal> {
    at.filter(|w| w.kind.class() == WaitingClass::Asking)
        .map(|w| Refusal::Asking(w.describe()))
}

fn cycle(
    at: &At<'_>,
    target: &Target,
    key: &str,
    want: &str,
    change_within: Duration,
) -> Result<std::result::Result<String, Refusal>> {
    let harness = target.harness;
    let read = || -> Result<Option<&'static str>> {
        Ok(harness.mode_on_screen(&tmux::capture_screen(at.tmux_server, &target.pane)?))
    };
    let mut start = None;
    for presses in 0..=MAX_PRESSES {
        let waiting = match presses {
            0 => target.at.clone(),
            _ => match find(at.project_root, at.scope, at.agent, at.tmux_server)? {
                Ok(found) => found.at,
                Err(refusal) => return Ok(Err(refusal)),
            },
        };
        if let Some(refusal) = asking(waiting.as_ref()) {
            return Ok(Err(refusal));
        }
        let Some(now) = read()? else {
            return Ok(Err(Refusal::ModeUnknown));
        };
        if now == want {
            return Ok(Ok(now.to_string()));
        }
        if presses == MAX_PRESSES || start == Some(now) {
            return Ok(Err(Refusal::Unavailable));
        }
        start.get_or_insert(now);
        tmux::send_key(at.tmux_server, &target.pane, key)?;
        let deadline = Instant::now() + change_within;
        while read()? == Some(now) {
            if Instant::now() >= deadline {
                return Ok(Err(Refusal::Unchanged));
            }
            std::thread::sleep(POLL);
        }
    }
    Ok(Err(Refusal::Unavailable))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::WaitingKind;
    use crate::testing::TestServer;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A feature whose `implementer` runs a stand-in for Claude Code that
    /// shows its input box with `footers[0]` below it, moves to the next of
    /// `footers` on ⇧Tab, and records every byte it is sent.
    struct Cycling {
        server: TestServer,
        project: PathBuf,
        received: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Cycling {
        fn new(footers: &[&str]) -> Self {
            let server = TestServer::new();
            let dir = tempdir().unwrap();
            let (project, _, name) = server.setup_project_with_feature(dir.path(), "login");
            let session = tmux::session_name(&name, "login");
            let received = project.join(".pm/received");
            let bin = project.join(".pm/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let script = bin.join("claude");
            std::fs::write(
                &script,
                format!(
                    "draw() {{ printf '\\033[H\\033[2J──── agent ─\\r\\n❯ \\r\\n────────\\r\\n%s\\r\\n' \"$1\"; }}\n\
                     stty raw -echo\n\
                     draw \"$1\"\n\
                     while :; do\n\
                       c=$(dd bs=1 count=1 2>/dev/null)\n\
                       printf '%s' \"$c\" >> {received}\n\
                       if [ \"$c\" = Z ]; then first=$1; shift; set -- \"$@\" \"$first\"; draw \"$1\"; fi\n\
                     done\n",
                    received = received.display()
                ),
            )
            .unwrap();
            let args: Vec<String> = footers.iter().map(|f| tmux::shell_quote(f)).collect();
            let command = format!("/bin/sh {} {}", script.display(), args.join(" "));
            let target = server.spawn_harness_agent(
                &project,
                &session,
                "login",
                "implementer",
                Harness::ClaudeCode,
                &command,
                Liveness::Busy,
            );
            server.wait_for_pane_text(&target, "❯");
            Self {
                server,
                project,
                received,
                _dir: dir,
            }
        }

        fn set(&self, mode: &str) -> std::result::Result<String, Refusal> {
            set_mode(
                &self.project,
                "login",
                "implementer",
                mode,
                self.server.name(),
            )
            .unwrap()
        }

        fn current(&self) -> Option<String> {
            current_mode(
                &self.project,
                "login",
                "implementer",
                Harness::ClaudeCode,
                self.server.name(),
            )
            .unwrap()
        }

        fn received(&self) -> String {
            std::fs::read_to_string(&self.received).unwrap_or_default()
        }
    }

    const CYCLE: &[&str] = &[
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
        "  ⏸ manual mode on",
        "  ⏵⏵ accept edits on (shift+tab to cycle)",
        "  ⏸ plan mode on (shift+tab to cycle)",
    ];

    #[test]
    fn the_cycle_is_walked_to_the_mode_asked_for() {
        let agent = Cycling::new(CYCLE);
        assert_eq!(agent.current().as_deref(), Some("auto"));

        assert_eq!(agent.set("acceptEdits"), Ok("acceptEdits".into()));

        assert_eq!(agent.current().as_deref(), Some("acceptEdits"));
        assert_eq!(agent.received(), "\x1b[Z\x1b[Z");
        assert_eq!(agent.set("acceptEdits"), Ok("acceptEdits".into()));
        assert_eq!(agent.received(), "\x1b[Z\x1b[Z");
    }

    #[test]
    fn a_mode_outside_the_cycle_is_unavailable_and_the_walk_ends_where_it_began() {
        let agent = Cycling::new(CYCLE);

        assert_eq!(agent.set("bypassPermissions"), Err(Refusal::Unavailable));

        assert_eq!(agent.current().as_deref(), Some("auto"));
        assert_eq!(agent.received(), "\x1b[Z".repeat(CYCLE.len()));
    }

    #[test]
    fn nothing_is_pressed_without_a_mode_on_screen_or_with_a_dialog_recorded() {
        let agent = Cycling::new(&["  ? for shortcuts"]);
        assert_eq!(agent.current(), None);
        assert_eq!(agent.set("plan"), Err(Refusal::ModeUnknown));

        let agent = Cycling::new(CYCLE);
        let waiting = Waiting::now(WaitingKind::Permission, None);
        runtime::write_waiting(&agent.project, "login", "implementer", &waiting).unwrap();
        assert_eq!(
            agent.set("plan"),
            Err(Refusal::Asking("permission prompt".into()))
        );

        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(agent.received(), "");
    }

    #[test]
    fn a_press_that_changes_nothing_is_reported() {
        let agent = Cycling::new(&[CYCLE[0]]);

        let at = At {
            project_root: &agent.project,
            scope: "login",
            agent: "implementer",
            tmux_server: agent.server.name(),
        };
        let set = set_mode_within(&at, "plan", Duration::from_millis(400)).unwrap();

        assert_eq!(set, Err(Refusal::Unchanged));
    }
}
