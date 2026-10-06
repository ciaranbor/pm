//! Input from the user that reaches an agent other than at its window:
//! text, an interrupt, or keys, typed into its pane as if at the keyboard
//! (`pm serve`'s write endpoints). Text is never sent as a continuation, so
//! its UserPromptSubmit resets a blocked feature and the transcript records
//! the user's words as the user's. A dialog a hook can see is answered
//! through its harness instead ([`super::hooks_dialog`]).
//!
//! Keys reach whatever the pane shows, so each send first checks the agent
//! is active, its pane runs its harness, and the pane is out of copy mode.
//! Text is then refused while a dialog is up — the paste's Enter would
//! answer it — and unless the input line reads as empty, once any key its
//! harness names to make the line take text has been pressed (vim NORMAL
//! mode): a draft at the keyboard is never merged into or cleared.
//!
//! An idle agent sits at its prompt, pm's waiter running beside it, so
//! text submits at once and Escape or a key does no harm. Text typed
//! mid-turn the harness holds until a step of the turn ends.

use std::path::Path;

use crate::error::Result;
use crate::harness::{Conversation, Harness};
use crate::state::agent::{self as registry, AgentRegistry};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::state::runtime::{self, SessionPath, Waiting, WaitingClass};
use crate::tmux::{self, Pane};

use super::running_agents::{AgentAt, Liveness, Windows, classify};

/// The keys a device may press, by tmux's names for them.
pub const KEYS: &[&str] = &[
    "Escape", "Enter", "Tab", "BTab", "Up", "Down", "Left", "Right", "Space", "BSpace", "C-c", "0",
    "1", "2", "3", "4", "5", "6", "7", "8", "9",
];

/// How typed text reached the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Submitted as a prompt of its own.
    Sent,
    /// Mid-turn: the harness holds it until a step of the turn ends.
    Queued,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Queued => "queued",
        }
    }
}

/// Text typed into an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub delivery: Delivery,
    /// Where the agent's conversation ended before it, for finding it there
    /// ([`said`]); `None` when it had none.
    pub after: Option<String>,
}

/// Why nothing was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not an active agent.
    Inactive,
    /// Its window is gone.
    NoWindow,
    /// Its pane runs no harness.
    NotRunning,
    /// A dialog is up; it is answered with keys.
    Asking(String),
    /// The input line holds a draft, or isn't on screen.
    NotAtPrompt,
}

impl Refusal {
    /// A stable name for the refusal, for a client to act on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::NoWindow => "no-window",
            Self::NotRunning => "not-running",
            Self::Asking(_) => "asking",
            Self::NotAtPrompt => "not-at-prompt",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inactive => f.write_str("the agent is not running"),
            Self::NoWindow => f.write_str("the agent has no window"),
            Self::NotRunning => f.write_str("the agent's window runs no harness"),
            Self::Asking(what) => write!(f, "a dialog is up ({what}); answer it with keys"),
            Self::NotAtPrompt => {
                f.write_str("the agent's input line is not empty, or not on screen")
            }
        }
    }
}

/// An agent whose pane runs its harness, found for a send.
struct Target {
    harness: Harness,
    pane: String,
    liveness: Liveness,
    /// What a busy one is at ([`classify`]).
    at: Option<Waiting>,
}

/// The pane of `agent`, once it is out of any tmux mode, if it runs the
/// agent's harness.
fn target(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Result<std::result::Result<Target, Refusal>> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let Some(entry) = registry.get(agent).filter(|e| e.active) else {
        return Ok(Err(Refusal::Inactive));
    };
    let project = ProjectConfig::load(&paths::pm_dir(project_root))?;
    let config = resolve_harness_config(&project.harness, &GlobalConfig::load_or_default().harness);
    let session = tmux::session_name(&project.project.name, scope);
    let windows = Windows::read(tmux_server)?;
    let Some(pane) = windows.find(&session, &entry.window_name) else {
        return Ok(Err(Refusal::NoWindow));
    };
    let processes = windows.processes(pane);
    let runs_harness = processes.as_deref().is_some_and(|ps| {
        ps.iter()
            .any(|p| entry.harness.runs_as(&p.command, &config))
    });
    let agent = AgentAt {
        project_root,
        scope,
        name: agent,
        harness: entry.harness,
    };
    let (state, at) = classify(agent, processes.as_deref(), &config);
    if state == Liveness::Dead || !runs_harness {
        return Ok(Err(Refusal::NotRunning));
    }
    out_of_mode(tmux_server, pane)?;
    Ok(Ok(Target {
        harness: entry.harness,
        pane: pane.id.clone(),
        liveness: state,
        at,
    }))
}

fn out_of_mode(tmux_server: Option<&str>, pane: &Pane) -> Result<()> {
    if tmux::paste::in_mode(tmux_server, &pane.id)? {
        tmux::paste::cancel_mode(tmux_server, &pane.id)?;
    }
    Ok(())
}

/// Type `text` into `agent`'s input line and submit it.
pub fn send_text(
    project_root: &Path,
    scope: &str,
    agent: &str,
    text: &str,
    tmux_server: Option<&str>,
) -> Result<std::result::Result<Typed, Refusal>> {
    let target = match target(project_root, scope, agent, tmux_server)? {
        Ok(target) => target,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let at = target.at;
    if let Some(asking) = at
        .as_ref()
        .filter(|w| w.kind.class() == WaitingClass::Asking)
    {
        return Ok(Err(Refusal::Asking(asking.describe())));
    }
    if !input_line_ready(
        project_root,
        scope,
        agent,
        target.harness,
        &target.pane,
        tmux_server,
    )? {
        return Ok(Err(Refusal::NotAtPrompt));
    }
    let after = conversation_end(project_root, scope, agent)?;
    let mid_turn = target.liveness == Liveness::Busy && at.is_none();
    tmux::paste::paste_text(tmux_server, &target.pane, text)?;
    let delivery = if mid_turn {
        Delivery::Queued
    } else {
        Delivery::Sent
    };
    Ok(Ok(Typed { delivery, after }))
}

/// Where `agent`'s conversation ends now; `None` when it has none.
pub(super) fn conversation_end(
    project_root: &Path,
    scope: &str,
    agent: &str,
) -> Result<Option<String>> {
    Ok(match registry::conversation(project_root, scope, agent)? {
        Some(conversation) => Some(conversation.page(None, 1)?.after),
        None => None,
    })
}

/// The SHA-256 of `text`, trimmed, in hex: what [`said`] matches.
pub fn sha256(text: &str) -> String {
    crate::hash::sha256_hex(text.trim().as_bytes())
}

/// Whether `conversation` took in the text whose [`sha256`] is
/// `text_sha256` as a prompt after cursor `after` ([`Conversation::took_in`]).
pub fn said(conversation: &Conversation, after: &str, text_sha256: &str) -> Result<bool> {
    conversation.took_in(after, |text| sha256(text) == text_sha256)
}

/// Press Escape in `agent`'s pane.
pub fn interrupt(
    project_root: &Path,
    scope: &str,
    agent: &str,
    tmux_server: Option<&str>,
) -> Result<std::result::Result<(), Refusal>> {
    let target = match target(project_root, scope, agent, tmux_server)? {
        Ok(target) => target,
        Err(refusal) => return Ok(Err(refusal)),
    };
    tmux::send_key(tmux_server, &target.pane, "Escape")?;
    Ok(Ok(()))
}

/// Press `keys`, each one of [`KEYS`], in `agent`'s pane, in order.
pub fn send_keys(
    project_root: &Path,
    scope: &str,
    agent: &str,
    keys: &[&str],
    tmux_server: Option<&str>,
) -> Result<std::result::Result<(), Refusal>> {
    let target = match target(project_root, scope, agent, tmux_server)? {
        Ok(target) => target,
        Err(refusal) => return Ok(Err(refusal)),
    };
    for key in keys {
        tmux::send_key(tmux_server, &target.pane, key)?;
    }
    Ok(Ok(()))
}

/// Type `text` into `agent`'s pane as keys, pressing nothing after it: for
/// a dialog that takes text, such as a login code.
pub fn type_text(
    project_root: &Path,
    scope: &str,
    agent: &str,
    text: &str,
    tmux_server: Option<&str>,
) -> Result<std::result::Result<(), Refusal>> {
    let target = match target(project_root, scope, agent, tmux_server)? {
        Ok(target) => target,
        Err(refusal) => return Ok(Err(refusal)),
    };
    tmux::send_literal(tmux_server, &target.pane, text)?;
    Ok(Ok(()))
}

/// Whether the input line in `pane` is empty and takes typed keys as text,
/// once any key its harness names to make it take text has been pressed;
/// that key is undone should the line have taken it as text.
fn input_line_ready(
    project_root: &Path,
    scope: &str,
    agent: &str,
    harness: Harness,
    pane: &str,
    tmux_server: Option<&str>,
) -> Result<bool> {
    let home = paths::home_dir()?;
    let config_dir = runtime::read_session_path(project_root, scope, agent, SessionPath::ConfigDir);
    let config_dir = config_dir.as_deref();
    let mut screen = tmux::capture_screen(tmux_server, pane)?;
    if let Some((key, undo)) = harness.text_mode_key(&screen, &home, config_dir) {
        tmux::send_key(tmux_server, pane, key)?;
        let takes_text = |s: &str| harness.input_is_empty(s, &home, config_dir).is_some();
        screen = redrawn(tmux_server, pane, takes_text)?;
        if !takes_text(&screen) {
            tmux::send_key(tmux_server, pane, undo)?;
            return Ok(false);
        }
    }
    Ok(harness.input_is_empty(&screen, &home, config_dir) == Some(true))
}

/// `pane`'s screen once `done` holds for it, or as it is after a second.
fn redrawn(tmux_server: Option<&str>, pane: &str, done: impl Fn(&str) -> bool) -> Result<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let screen = tmux::capture_screen(tmux_server, pane)?;
        if done(&screen) || std::time::Instant::now() >= deadline {
            return Ok(screen);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::runtime::{Waiting, WaitingKind};
    use crate::testing::TestServer;
    use std::path::PathBuf;
    use tempfile::tempdir;

    const TEXT: &str = "first line\nquote \" and ' and $HOME; `ls` \\ end\n";

    /// A feature whose `implementer` runs `harness` as a program that shows
    /// Claude Code's empty input box and records every byte its pane is
    /// sent to `received`.
    struct Recording {
        server: TestServer,
        project: PathBuf,
        target: String,
        received: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Recording {
        fn new(harness: Harness) -> Self {
            let server = TestServer::new();
            let dir = tempdir().unwrap();
            let (project, name) = server.setup_project_with_feature(dir.path(), "login");
            let session = tmux::session_name(&name, "login");
            let (target, received) =
                server.spawn_recording_agent(&project, &session, "login", "implementer", harness);
            Self {
                server,
                project,
                target,
                received,
                _dir: dir,
            }
        }

        fn send(&self, text: &str) -> std::result::Result<Delivery, Refusal> {
            send_text(
                &self.project,
                "login",
                "implementer",
                text,
                self.server.name(),
            )
            .unwrap()
            .map(|typed| typed.delivery)
        }

        /// What the program has read once it has read `len` bytes, or after
        /// a few seconds.
        fn received(&self, len: usize) -> Vec<u8> {
            let mut bytes = Vec::new();
            for _ in 0..250 {
                bytes = std::fs::read(&self.received).unwrap_or_default();
                if bytes.len() >= len {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            bytes
        }

        fn mark(&self, kind: WaitingKind) {
            let waiting = Waiting::now(kind, None);
            runtime::write_waiting(&self.project, "login", "implementer", &waiting).unwrap();
        }

        /// Mark the agent idle, its pane's program standing for its waiter.
        fn idle(&self) {
            let processes = tmux::pane_processes(self.server.name(), &self.target).unwrap();
            let waiter = processes.last().unwrap().pid;
            runtime::take_waiter(&self.project, "login", "implementer", waiter, None).unwrap();
            self.mark(WaitingKind::Idle);
        }
    }

    fn pasted(text: &str) -> Vec<u8> {
        format!("\x1b[200~{text}\x1b[201~\r").into_bytes()
    }

    #[test]
    fn text_reaches_a_busy_agent_byte_for_byte_queued() {
        let agent = Recording::new(Harness::ClaudeCode);

        assert_eq!(agent.send(TEXT), Ok(Delivery::Queued));

        let want = pasted(TEXT);
        assert_eq!(agent.received(want.len()), want);
    }

    #[test]
    fn an_agent_at_its_prompt_takes_the_text_at_once() {
        let agent = Recording::new(Harness::ClaudeCode);
        agent.mark(WaitingKind::Prompt);

        assert_eq!(agent.send("hello"), Ok(Delivery::Sent));

        let want = pasted("hello");
        assert_eq!(agent.received(want.len()), want);
    }

    #[test]
    fn an_idle_agent_takes_text_at_once_and_keys() {
        let agent = Recording::new(Harness::ClaudeCode);
        agent.idle();

        assert_eq!(agent.send("hello"), Ok(Delivery::Sent));
        let want = pasted("hello");
        assert_eq!(agent.received(want.len()), want);

        let interrupted = interrupt(&agent.project, "login", "implementer", agent.server.name());
        assert_eq!(interrupted.unwrap(), Ok(()));
        let mut want = want;
        want.push(0x1b);
        assert_eq!(agent.received(want.len()), want);
    }

    fn a_pane_in_a_mode_leaves_it_and_takes_the_text(enter: &str) {
        let agent = Recording::new(Harness::ClaudeCode);
        agent.server.tmux_stdout(&[enter, "-t", &agent.target]);
        let pane = agent.server.pane_id(&agent.target);
        assert!(tmux::paste::in_mode(agent.server.name(), &pane).unwrap());

        assert_eq!(agent.send("hello"), Ok(Delivery::Queued));

        let want = pasted("hello");
        assert_eq!(agent.received(want.len()), want);
        assert!(!tmux::paste::in_mode(agent.server.name(), &pane).unwrap());
    }

    #[test]
    fn a_pane_in_copy_mode_leaves_it_and_takes_the_text() {
        a_pane_in_a_mode_leaves_it_and_takes_the_text("copy-mode");
    }

    #[test]
    fn a_pane_in_tree_mode_leaves_it_and_takes_the_text() {
        a_pane_in_a_mode_leaves_it_and_takes_the_text("choose-tree");
    }

    #[test]
    fn nothing_is_typed_while_a_dialog_is_up_but_keys_are() {
        let agent = Recording::new(Harness::ClaudeCode);
        agent.mark(WaitingKind::Permission);

        assert_eq!(
            agent.send("hello"),
            Err(Refusal::Asking("permission prompt".into()))
        );

        let pressed = send_keys(
            &agent.project,
            "login",
            "implementer",
            &["1"],
            agent.server.name(),
        )
        .unwrap();
        assert_eq!(pressed, Ok(()));
        assert_eq!(agent.received(1), b"1");
    }

    #[test]
    fn a_draft_at_the_keyboard_is_left_untouched() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&name, "login");
        let target = server.spawn_prompting_fake_agent(&project, &session, "login", "implementer");
        server.wait_for_pane_text(&target, "❯");
        tmux::send_key(server.name(), &target, "h").unwrap();
        server.wait_for_pane_text(&target, "❯ h");

        let sent = send_text(&project, "login", "implementer", "hello", server.name()).unwrap();

        assert_eq!(sent, Err(Refusal::NotAtPrompt));
        let screen = tmux::capture_pane(server.name(), &target).unwrap();
        assert!(!screen.contains("hello"), "{screen}");
    }

    #[test]
    fn a_pane_without_its_harness_gets_nothing() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&name, "login");
        let target = server.spawn_dead_fake_agent(&project, &session, "login", "implementer");

        let sent = send_text(&project, "login", "implementer", "hello", server.name()).unwrap();
        let interrupted = interrupt(&project, "login", "implementer", server.name()).unwrap();

        assert_eq!(sent, Err(Refusal::NotRunning));
        assert_eq!(interrupted, Err(Refusal::NotRunning));
        let screen = tmux::capture_pane(server.name(), &target).unwrap();
        assert!(!screen.contains("hello"), "{screen}");
    }

    #[test]
    fn typed_text_reaches_a_dialog_as_keys_with_nothing_pressed_after() {
        let agent = Recording::new(Harness::ClaudeCode);
        agent.mark(WaitingKind::Startup);
        let code = "-t Enter ab#1é";

        let typed = type_text(
            &agent.project,
            "login",
            "implementer",
            code,
            agent.server.name(),
        )
        .unwrap();

        assert_eq!(typed, Ok(()));
        assert_eq!(agent.received(code.len()), code.as_bytes());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(std::fs::read(&agent.received).unwrap(), code.as_bytes());
    }

    #[test]
    fn a_busy_agent_is_interrupted_with_escape() {
        let agent = Recording::new(Harness::ClaudeCode);

        let interrupted =
            interrupt(&agent.project, "login", "implementer", agent.server.name()).unwrap();

        assert_eq!(interrupted, Ok(()));
        assert_eq!(agent.received(1), b"\x1b");
    }
}
