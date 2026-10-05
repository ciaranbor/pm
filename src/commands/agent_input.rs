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
//! mode): a draft at the keyboard is never merged into or cleared. A
//! harness whose input line pm can't read takes the text as it is.
//!
//! An agent waiting in pm's Stop hook gets the text queued behind the hook
//! on a harness that [holds it](Harness::holds_input_behind_stop_hook), so
//! a yield request goes first ([`runtime::request_yield`]); it is written
//! before the paste, so a turn that ends meanwhile yields too, and only for
//! an agent whose next turn end runs the hook — one at its prompt submits
//! the text at once. Text typed mid-turn may be taken in at a step's end
//! instead; the hook finds it in the conversation ([`said`]) and waits on.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::harness::transcript::items::{Body, Item};
use crate::harness::{Conversation, Harness};
use crate::state::agent::{self as registry, AgentRegistry};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig, resolve_harness_config};
use crate::state::runtime::{self, SessionPath, Waiting, WaitingClass, YieldRequest};
use crate::tmux::{self, Pane};

use super::running_agents::{Liveness, Windows, liveness, waiting};

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
    /// It waits for a message in pm's Stop hook, which a key would end.
    Idle,
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
            Self::Idle => "idle",
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
            Self::Idle => f.write_str("the agent is between turns, waiting for a message"),
        }
    }
}

/// An agent whose pane runs its harness, found for a send.
struct Target {
    harness: Harness,
    pane: String,
    liveness: Liveness,
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
    let state = liveness(processes.as_deref(), entry.harness, &config);
    if state == Liveness::Dead || !runs_harness {
        return Ok(Err(Refusal::NotRunning));
    }
    out_of_mode(tmux_server, pane)?;
    Ok(Ok(Target {
        harness: entry.harness,
        pane: pane.id.clone(),
        liveness: state,
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
    let at = match target.liveness {
        Liveness::Busy => waiting(project_root, scope, agent, target.harness),
        _ => None,
    };
    if let Some(asking) = at
        .as_ref()
        .filter(|w| w.kind.class() == WaitingClass::Asking)
    {
        return Ok(Err(Refusal::Asking(asking.describe())));
    }
    if target.harness.reads_input_line()
        && !input_line_ready(
            project_root,
            scope,
            agent,
            target.harness,
            &target.pane,
            tmux_server,
        )?
    {
        return Ok(Err(Refusal::NotAtPrompt));
    }
    let after = conversation_end(project_root, scope, agent)?;
    let mid_turn = target.liveness == Liveness::Busy && at.is_none();
    let requested = target.harness.holds_input_behind_stop_hook()
        && hook_runs_next(target.liveness, at.as_ref());
    if requested {
        request_yield(project_root, scope, agent, text, after.clone())?;
    }
    if let Err(e) = tmux::paste::paste_text(tmux_server, &target.pane, text) {
        if requested {
            let _ = runtime::take_yield_request(project_root, scope, agent);
        }
        return Err(e);
    }
    let delivery = if mid_turn {
        Delivery::Queued
    } else {
        Delivery::Sent
    };
    Ok(Ok(Typed { delivery, after }))
}

/// Whether pm's Stop hook runs at the next turn end of an agent its window
/// reads as `liveness`, at `at` ([`waiting`]): it runs now, or the agent is
/// mid-turn, at no waiting marker or recorded turn end.
pub(super) fn hook_runs_next(liveness: Liveness, at: Option<&Waiting>) -> bool {
    liveness == Liveness::Idle || (liveness == Liveness::Busy && at.is_none())
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

/// Ask `agent`'s next Stop hook to yield for `text`, typed once its
/// conversation ended at `after` ([`runtime::request_yield`]).
pub(super) fn request_yield(
    project_root: &Path,
    scope: &str,
    agent: &str,
    text: &str,
    after: Option<String>,
) -> Result<()> {
    let request = YieldRequest {
        text_sha256: sha256(text),
        after,
    };
    runtime::request_yield(project_root, scope, agent, &request)
}

/// The SHA-256 of `text`, trimmed, in hex: what [`said`] matches.
pub fn sha256(text: &str) -> String {
    crate::hash::sha256_hex(text.trim().as_bytes())
}

/// Whether `items` hold the user saying `text` at or after `since`.
pub fn said_since(items: &[Item], since: DateTime<Utc>, text: &str) -> bool {
    let text_sha256 = sha256(text);
    items.iter().any(|item| {
        item.at.is_some_and(|at| at >= since)
            && matches!(&item.body, Body::User { text } if sha256(text) == text_sha256)
    })
}

/// Whether `conversation` took in the text whose [`sha256`] is
/// `text_sha256` as a prompt after cursor `after` ([`Conversation::took_in`]).
pub fn said(conversation: &Conversation, after: &str, text_sha256: &str) -> Result<bool> {
    conversation.took_in(after, |text| sha256(text) == text_sha256)
}

/// Press Escape in `agent`'s pane, unless it waits between turns: there
/// Escape would end pm's Stop hook and leave the agent unarmed.
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
    if target.liveness == Liveness::Idle {
        return Ok(Err(Refusal::Idle));
    }
    tmux::send_key(tmux_server, &target.pane, "Escape")?;
    Ok(Ok(()))
}

/// Press `keys`, each one of [`KEYS`], in `agent`'s pane, in order, unless
/// it waits between turns: nothing is on screen to answer, and Escape or
/// Ctrl-C would end pm's Stop hook.
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
    if target.liveness == Liveness::Idle {
        return Ok(Err(Refusal::Idle));
    }
    for key in keys {
        tmux::send_key(tmux_server, &target.pane, key)?;
    }
    Ok(Ok(()))
}

/// Whether the input line in `pane` is empty and takes typed keys as text,
/// once any key its harness names to make it take text has been pressed;
/// that key is undone should the line have taken it as text.
pub(super) fn input_line_ready(
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
    use crate::commands::running_agents::Liveness;
    use crate::state::runtime::{Waiting, WaitingKind};
    use crate::testing::{TestServer, fake_harness_binary};
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
            let received = dir.path().join("received");
            let cat = fake_harness_binary(harness, Path::new("/bin/cat"));
            let script = dir.path().join("recorder");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nclear\nprintf '\\033[?2004h──── agent ─\\n❯ \\n────────\\n\\033[2A\\033[3G'\n\
                     stty raw -echo\nexec {} -u > {}\n",
                    cat.display(),
                    received.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let session = tmux::session_name(&name, "login");
            let target = server.spawn_harness_agent(
                &project,
                &session,
                "login",
                "implementer",
                harness,
                &script.display().to_string(),
                Liveness::Busy,
            );
            server.wait_for_pane_text(&target, "❯");
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

        fn yield_requested(&self) -> bool {
            runtime::yield_requested(&self.project, "login", "implementer")
        }
    }

    fn pasted(text: &str) -> Vec<u8> {
        format!("\x1b[200~{text}\x1b[201~\r").into_bytes()
    }

    #[test]
    fn text_reaches_a_busy_agent_byte_for_byte_and_asks_its_stop_hook_to_yield() {
        let agent = Recording::new(Harness::ClaudeCode);

        assert_eq!(agent.send(TEXT), Ok(Delivery::Queued));

        let want = pasted(TEXT);
        assert_eq!(agent.received(want.len()), want);
        assert!(agent.yield_requested());
    }

    #[test]
    fn an_agent_at_its_prompt_takes_the_text_at_once_with_no_yield() {
        let agent = Recording::new(Harness::ClaudeCode);
        agent.mark(WaitingKind::Prompt);

        assert_eq!(agent.send("hello"), Ok(Delivery::Sent));

        let want = pasted("hello");
        assert_eq!(agent.received(want.len()), want);
        assert!(!agent.yield_requested());
    }

    #[test]
    fn an_opencode_agent_is_never_asked_to_yield() {
        let agent = Recording::new(Harness::OpenCode);

        assert_eq!(agent.send("hello"), Ok(Delivery::Queued));

        let want = pasted("hello");
        assert_eq!(agent.received(want.len()), want);
        assert!(!agent.yield_requested());
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
        assert!(!agent.yield_requested());

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
        assert!(!runtime::yield_requested(&project, "login", "implementer"));
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
    fn an_agent_between_turns_is_neither_interrupted_nor_sent_keys() {
        let server = TestServer::new();
        let dir = tempdir().unwrap();
        let (project, name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&name, "login");
        let shell = fake_harness_binary(Harness::ClaudeCode, Path::new("/bin/bash"));
        let command = format!(
            "{} -c \"sh -c 'sleep 999; :' {}; :\"",
            shell.display(),
            crate::commands::hooks_install::PM_HOOK_MARKER
        );
        server.spawn_harness_agent(
            &project,
            &session,
            "login",
            "implementer",
            Harness::ClaudeCode,
            &command,
            Liveness::Idle,
        );

        let interrupted = interrupt(&project, "login", "implementer", server.name()).unwrap();
        let pressed =
            send_keys(&project, "login", "implementer", &["Escape"], server.name()).unwrap();

        assert_eq!(interrupted, Err(Refusal::Idle));
        assert_eq!(pressed, Err(Refusal::Idle));
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
