//! The agent harness: the CLI that pm launches inside a tmux window to run an
//! agent. Everything harness-specific — command shape, capability probes,
//! where it reads agent definitions and skills from, trust it needs before it
//! will run pm's hooks — sits behind a `match` on [`Harness`] here, so the
//! spawn chokepoint, the asset installer, the hooks, and the registry stay
//! harness-neutral. Per-agent settings that only the harness can interpret
//! (model id, permission mode) are passed through verbatim.
//!
//! Session portability (`pm harness migrate|export|import`) is three seams
//! over each harness's own store. All of them preserve session ids, so a
//! registry entry stays valid across a move; none opens a harness's database.

mod claude_code;
mod codex;
mod hook_trust;
mod opencode;
pub(crate) mod probe;
mod projection;
mod screen;
mod session_store;
mod spawn;
pub(crate) mod transcript;

pub use hook_trust::{HookTrust, HookTrustStatus};
#[cfg(test)]
pub(crate) use opencode::chat::testing as opencode_testing;
pub use probe::Probe;
pub(crate) use projection::project_by_copy;
pub use projection::{Projection, ProjectionScope};
pub use session_store::{
    AgentSession, Conversation, ExportJob, ImportOutcome, InUse, SessionStore,
};
pub(crate) use session_store::{per_session_outcome, session_counts};
pub use spawn::{LaunchContext, PreLaunch, SpawnSpec, resumable_session};

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::{AgentsConfig, HarnessConfig, layered};
use crate::state::runtime::{
    self, Answer, Dialog, DialogRecord, SessionPath, Waiting, WaitingClass, WaitingKind,
};

use session_store::Location;

/// A change to an agent's waiting marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitingEvent {
    /// Replace any marker.
    Set(Waiting),
    /// Set unless the agent already has a marker whose class isn't in
    /// `over`: a vaguer report must not hide a more specific one.
    Fill {
        waiting: Waiting,
        over: &'static [WaitingClass],
    },
    /// The agent is working again.
    Clear,
    /// A subagent is working again: clear a marker it set, and only that.
    ClearSubagent(String),
}

/// `text`'s first line, cut to a length that fits a status line.
pub(crate) fn one_line(text: &str) -> String {
    const MAX: usize = 120;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    match line.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// Which agent CLI a spawn runs on. Dispatched by `match` per seam rather
/// than a trait: the variant set is small and closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    #[default]
    ClaudeCode,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
}

impl Harness {
    /// The string form used in config, the registry, and `pm agent list`.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
        }
    }

    /// Every harness pm can spawn, in the order shown in error messages.
    pub const SUPPORTED: &[Harness] = &[Harness::ClaudeCode, Harness::Codex, Harness::OpenCode];

    /// What must happen before the harness's command runs, beyond
    /// [`trust_worktree`](Self::trust_worktree): anything the command line
    /// depends on that only the harness can produce (opencode: the session
    /// the TUI opens). Runs at the spawn chokepoint before the window
    /// exists, so a failure leaves nothing behind.
    pub fn pre_launch(
        self,
        ctx: &LaunchContext<'_>,
        spec: &SpawnSpec<'_>,
        config: &HarnessConfig,
    ) -> Result<PreLaunch> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Ok(PreLaunch::default()),
            Harness::OpenCode => opencode::pre_launch(ctx, spec, &config.opencode),
        }
    }

    /// The command line that launches an agent for `spec`, with the
    /// harness's own `[harness.<name>]` settings from `config` and what
    /// [`pre_launch`](Self::pre_launch) prepared.
    pub fn build_cmd(
        self,
        spec: &SpawnSpec<'_>,
        config: &HarnessConfig,
        pre: &PreLaunch,
    ) -> String {
        match self {
            Harness::ClaudeCode => claude_code::build_cmd(spec),
            Harness::Codex => codex::build_cmd(spec, &config.codex),
            Harness::OpenCode => opencode::build_cmd(spec, &config.opencode, pre),
        }
    }

    /// Whether the installed harness binary can deliver pm's composed prompt
    /// (the shared baseline and notice boards) to a spawned agent — see
    /// [`prompt_mechanism`](Self::prompt_mechanism). `None` when the binary
    /// can't be probed at all.
    pub fn supports_prompt_delivery(self, config: &HarnessConfig, probe: Probe) -> Option<bool> {
        match self {
            Harness::ClaudeCode => claude_code::supports_append_file(probe),
            Harness::Codex => codex::version_supported(probe),
            Harness::OpenCode => opencode::version_supported(&config.opencode, probe),
        }
    }

    /// The installed binary's `--version` answer, trimmed; `None` when it
    /// can't be run.
    pub fn installed_version(self, config: &HarnessConfig, probe: Probe) -> Option<String> {
        match self {
            Harness::ClaudeCode => claude_code::installed_version(probe),
            Harness::Codex => codex::installed_version(probe),
            Harness::OpenCode => opencode::installed_version(&config.opencode, probe).ok(),
        }
    }

    /// The oldest release pm runs agents on; `None` when pm needs none in
    /// particular.
    pub fn min_version(self) -> Option<String> {
        match self {
            Harness::ClaudeCode => None,
            Harness::Codex => Some(codex::min_version_string()),
            Harness::OpenCode => Some(opencode::min_version_string()),
        }
    }

    /// The manual step that gives a new machine this harness's credentials,
    /// which no pm export carries.
    pub fn credentials_step(self, config: &HarnessConfig) -> String {
        match self {
            Harness::ClaudeCode => claude_code::CREDENTIALS_STEP.to_string(),
            Harness::Codex => codex::CREDENTIALS_STEP.to_string(),
            Harness::OpenCode => opencode::credentials_step(&config.opencode),
        }
    }

    /// Why no agent can run on this harness as installed: its binary can't
    /// be run, or is older than the release pm's command line or never-idle
    /// loop needs. `None` when it can.
    pub fn unusable_reason(self, config: &HarnessConfig, probe: Probe) -> Option<String> {
        match self {
            // Lib tests must not depend on what the machine has installed.
            Harness::ClaudeCode | Harness::Codex if cfg!(test) => None,
            Harness::ClaudeCode => claude_code::unusable_reason(probe),
            Harness::Codex => codex::unusable_reason(probe),
            Harness::OpenCode => opencode::unusable_reason(&config.opencode, probe),
        }
    }

    /// Whether an agent on this harness is refused without an
    /// `[agents.models]` row.
    pub fn requires_model(self) -> bool {
        match self {
            Harness::ClaudeCode | Harness::Codex => false,
            Harness::OpenCode => true,
        }
    }

    /// What is wrong with an agent's `[agents.models]` and
    /// `[agents.permissions]` rows, for a harness that refuses a row it
    /// cannot take instead of passing it through.
    pub fn row_issues(self, model: Option<&str>, permission_mode: Option<&str>) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::row_issues(model, permission_mode),
        }
    }

    /// What a spawn remarks on about an agent's `[agents.models]` row
    /// without refusing it.
    pub fn row_notes(self, config: &HarnessConfig, model: Option<&str>) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::row_notes(&config.opencode, model),
        }
    }

    /// What is wrong with the harness's `[harness.<name>]` settings for
    /// agents started in `worktree`, where only the harness can tell. `rows`
    /// are the `[agents.models]` rows of the agents on this harness.
    pub fn config_issues(
        self,
        config: &HarnessConfig,
        worktree: &Path,
        rows: &[String],
    ) -> Vec<ConfigIssue> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::config_issues(&config.opencode, worktree, rows),
        }
    }

    /// How the composed prompt reaches an agent, for probe and doctor
    /// messages.
    pub fn prompt_mechanism(self) -> String {
        match self {
            Harness::ClaudeCode => {
                "appending a prompt file (--append-system-prompt-file)".to_string()
            }
            Harness::Codex => format!(
                "SessionStart hook context injection outside the shared daemon \
                 (needs codex >= {})",
                codex::min_version_string()
            ),
            Harness::OpenCode => format!(
                "the pm-never-idle plugin's context hook (needs opencode >= {})",
                opencode::min_version_string()
            ),
        }
    }

    /// The harness's own config directory, relative to a worktree or home
    /// (`.claude` for claude-code). Agent definitions and skills are
    /// projected from the canonical `.agents/` store into it.
    pub fn config_dir(self) -> &'static str {
        match self {
            Harness::ClaudeCode => claude_code::CONFIG_DIR,
            Harness::Codex => codex::CONFIG_DIR,
            Harness::OpenCode => opencode::CONFIG_DIR,
        }
    }

    /// The harness's global config dir under `home` (`~/.claude` for
    /// claude-code, `$CODEX_HOME` or `~/.codex` for codex,
    /// `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode` for opencode), where the global
    /// canonical store is projected and the user-level settings live. `None`
    /// for a harness with no global dir — such a harness would need the
    /// global assets projected into each project's own dir instead, which
    /// no current harness requires.
    pub fn global_config_dir(self, home: &Path) -> Option<PathBuf> {
        match self {
            Harness::ClaudeCode => Some(home.join(claude_code::CONFIG_DIR)),
            Harness::Codex => Some(codex::home_dir(home)),
            Harness::OpenCode => Some(opencode::global_dir(home)),
        }
    }

    /// The harness's user-level file that pm installs its hooks into, once
    /// per machine (`~/.claude/settings.json`, `$CODEX_HOME/hooks.json`).
    /// Both take the same nested `hooks` shape. `None` for a harness
    /// whose loop is not a hooks entry; see
    /// [`plugin_files`](Self::plugin_files).
    pub fn user_settings_file(self, home: &Path) -> Option<PathBuf> {
        let dir = self.global_config_dir(home)?;
        match self {
            Harness::ClaudeCode => Some(dir.join(claude_code::USER_SETTINGS_FILE)),
            Harness::Codex => Some(dir.join(codex::HOOKS_FILE)),
            Harness::OpenCode => None,
        }
    }

    /// This harness's own `[harness.<name>]` section of `config`.
    pub fn config_section(self, config: &HarnessConfig) -> serde_json::Value {
        match self {
            Harness::ClaudeCode => serde_json::Value::Null,
            Harness::Codex => serde_json::to_value(&config.codex).unwrap_or_default(),
            Harness::OpenCode => serde_json::to_value(&config.opencode).unwrap_or_default(),
        }
    }

    /// Whether a running session picks up an edit to pm's never-idle loop
    /// — its hook entries or plugin files — without a restart.
    pub fn hooks_reload_live(self) -> bool {
        match self {
            Harness::ClaudeCode => true,
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// Files pm owns outright in the harness's user-level dir and writes
    /// verbatim, once per machine, with their bundled content — the
    /// counterpart of [`user_settings_file`](Self::user_settings_file) for a
    /// harness whose never-idle loop is a plugin. Empty for the others.
    pub fn plugin_files(self, home: &Path) -> Vec<(PathBuf, &'static str)> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::plugin_files(home),
        }
    }

    /// Whether the input line on this harness's `screen`, captured with its
    /// escape sequences, is empty and takes typed keys as text; `None` when
    /// it can't tell. `home` holds the harness's config.
    ///
    /// `config_dir` is the value the agent's environment gave
    /// [`config_dir_env`](Self::config_dir_env), if any.
    pub fn input_is_empty(
        self,
        screen: &str,
        home: &Path,
        config_dir: Option<&Path>,
    ) -> Option<bool> {
        match self {
            Harness::ClaudeCode => claude_code::input::is_empty(screen, home, config_dir),
            Harness::Codex => codex::input::is_empty(screen),
            Harness::OpenCode => opencode::input::is_empty(screen),
        }
    }

    /// The tmux key that switches an input line on `screen` into a mode that
    /// takes typed keys as text, changing nothing else, and the one that
    /// erases it should the line have taken it as text; `None` when it
    /// already does or can't. Arguments as for
    /// [`input_is_empty`](Self::input_is_empty).
    pub fn text_mode_key(
        self,
        screen: &str,
        home: &Path,
        config_dir: Option<&Path>,
    ) -> Option<(&'static str, &'static str)> {
        match self {
            Harness::ClaudeCode => claude_code::input::text_mode_key(screen, home, config_dir),
            Harness::Codex => codex::input::text_mode_key(screen),
            Harness::OpenCode => None,
        }
    }

    /// How pm's Stop hook, installed for this harness, gets the
    /// continuation to the agent.
    pub fn wake(self) -> Wake {
        match self {
            Harness::ClaudeCode => Wake::Rewake,
            Harness::Codex => Wake::Queue,
            Harness::OpenCode => Wake::Block,
        }
    }

    /// The keys pm's Stop hook entry carries beyond its command and
    /// timeout, which make the harness run it once the turn has ended.
    pub fn stop_hook_options(self) -> serde_json::Map<String, serde_json::Value> {
        match self {
            Harness::ClaudeCode => claude_code::STOP_HOOK_OPTIONS,
            Harness::Codex => codex::STOP_HOOK_OPTIONS,
            Harness::OpenCode => &[],
        }
        .iter()
        .map(|(key, value)| (key.to_string(), serde_json::Value::Bool(*value)))
        .collect()
    }

    /// Put `text` on the queue of the session `session_id`, which starts it
    /// as a turn once the session is idle ([`Wake::Queue`]).
    pub fn queue_prompt(self, session_id: &str, text: &str) -> Result<()> {
        match self {
            Harness::Codex => codex::queue_prompt(session_id, text),
            Harness::ClaudeCode | Harness::OpenCode => {
                Err(PmError::Agent(format!("{self} has no prompt queue")))
            }
        }
    }

    /// What a prompt UserPromptSubmit reports says: the continuation of a
    /// Stop hook's [rewake](Wake::Rewake) comes wrapped.
    pub fn prompt_said(self, prompt: &str) -> &str {
        match self {
            Harness::ClaudeCode => claude_code::rewake_reason(prompt).unwrap_or(prompt),
            Harness::Codex | Harness::OpenCode => prompt,
        }
    }

    /// Whether this harness's waiter, alive, wakes an agent at `kind`. A
    /// rewake reaches a session however its turn ended; codex's queue
    /// skips an interrupted thread; a waiter that blocks inside the turn
    /// waits only where it marked the agent idle.
    pub fn waiter_wakes(self, kind: WaitingKind) -> bool {
        match self.wake() {
            Wake::Rewake => matches!(
                kind,
                WaitingKind::Idle
                    | WaitingKind::Interrupted
                    | WaitingKind::Error
                    | WaitingKind::Prompt
            ),
            Wake::Queue | Wake::Block => kind == WaitingKind::Idle,
        }
    }

    /// Whether `prompt`, as UserPromptSubmit reports it, is one the harness
    /// wrote itself rather than the user typed.
    pub fn synthesized_prompt(self, prompt: &str) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::chat::is_synthesized(prompt),
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// The environment variable that moves the harness's config away from
    /// its default, where the input line's settings are read, for the
    /// SessionStart hook to record from the agent's own environment.
    pub fn config_dir_env(self) -> Option<&'static str> {
        match self {
            Harness::ClaudeCode => Some(claude_code::input::CONFIG_DIR_ENV),
            Harness::Codex | Harness::OpenCode => None,
        }
    }

    /// How the last turn of the session whose transcript is at `transcript`
    /// ended, when it ended in a way the harness fires no hook for —
    /// interrupted, or failed — and nothing has happened in it since; dated
    /// by the transcript's mtime.
    pub fn turn_ended(self, transcript: &Path) -> Option<Waiting> {
        match self {
            Harness::ClaudeCode => claude_code::transcript::turn_ended(transcript),
            Harness::Codex => codex::transcript::turn_ended(transcript),
            Harness::OpenCode => None,
        }
    }

    /// Where the conversation of `agent`'s current session is read from;
    /// `None` until it has one. The transcript path the session reported at
    /// its start is preferred to the one derived from its id.
    pub fn conversation(self, agent: &AgentSession<'_>) -> Option<Conversation> {
        let recorded = || {
            runtime::read_session_path(
                agent.project_root,
                agent.scope,
                agent.name,
                SessionPath::Transcript,
            )
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(agent.session_id))
            })
        };
        let jsonl = |path, parse| Location::Jsonl { path, parse };
        let location = match self {
            Harness::ClaudeCode => {
                let config_dir = runtime::read_session_path(
                    agent.project_root,
                    agent.scope,
                    agent.name,
                    SessionPath::ConfigDir,
                );
                jsonl(
                    recorded().or_else(|| {
                        claude_code::chat::transcript_path(agent, config_dir.as_deref())
                    })?,
                    claude_code::chat::parse,
                )
            }
            Harness::Codex => jsonl(
                recorded().or_else(|| {
                    codex::sessions::find_rollout(&codex::home_dir(agent.home), agent.session_id)
                })?,
                codex::chat::parse,
            ),
            Harness::OpenCode => {
                let db = opencode::chat::db_path(agent.home, paths::data_home().as_deref());
                if agent.session_id.is_empty() || !db.is_file() {
                    return None;
                }
                Location::Session {
                    db,
                    id: agent.session_id.to_string(),
                }
            }
        };
        Some(Conversation {
            harness: self,
            location,
        })
    }

    /// Why this agent's never-idle loop stopped itself, if it did: pm's
    /// waiter records it, opencode's plugin in a file of its own.
    pub fn loop_stopped(self, project_root: &Path, scope: &str, agent: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => {
                runtime::loop_tripped(project_root, scope, agent)
            }
            Harness::OpenCode => {
                let file = opencode::trip_file(project_root, scope, agent).ok()?;
                let reason = std::fs::read_to_string(file).ok()?;
                Some(reason.trim().to_string())
            }
        }
    }

    /// The hook events, beyond the never-idle loop's, whose payloads say
    /// when the agent waits on the user ([`waiting_event`](Self::waiting_event)).
    /// Each harness's own: an event the other lacks may make it reject the
    /// file. Empty for a harness whose loop is a plugin, which reports them
    /// itself.
    pub fn waiting_events(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::waiting::EVENTS,
            Harness::Codex => codex::waiting::EVENTS,
            Harness::OpenCode => &[],
        }
    }

    /// What a hook payload from this harness says about the agent's waiting
    /// marker; `None` when nothing.
    pub fn waiting_event(self, payload: &serde_json::Value) -> Option<WaitingEvent> {
        match self {
            Harness::ClaudeCode => claude_code::waiting::event(payload),
            Harness::Codex => codex::waiting::event(payload),
            Harness::OpenCode => opencode::waiting::event(payload),
        }
    }

    /// The hook events whose payloads open a dialog the user can answer
    /// remotely ([`dialog`](Self::dialog)), for `pm harness hooks dialog`.
    /// Empty for a harness whose loop is a plugin, which calls it itself,
    /// and for codex, which runs its hooks before showing a dialog, so a
    /// hook waiting on the phone would hide the terminal's.
    pub fn dialog_events(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => &["PermissionRequest"],
            Harness::Codex | Harness::OpenCode => &[],
        }
    }

    /// The dialog a payload of `pm harness hooks dialog` opens, with what
    /// [`dialog_decision`](Self::dialog_decision) will need; `None` when
    /// none the user can answer remotely.
    pub fn dialog(self, payload: &serde_json::Value) -> Option<(Dialog, serde_json::Value)> {
        match self {
            Harness::ClaudeCode => claude_code::dialog::dialog(payload),
            Harness::Codex => None,
            Harness::OpenCode => opencode::dialog::dialog(payload),
        }
    }

    /// Whether a payload of `pm harness hooks waiting` says `record`'s
    /// dialog was answered at the terminal. Only Claude Code needs it: it
    /// leaves the hook of a dialog approved there running, while opencode's
    /// plugin ends the hook of an ask settled at the TUI.
    pub fn dialog_resolved(self, record: &DialogRecord, payload: &serde_json::Value) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::dialog::resolved(record, payload),
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// What the dialog hook prints for `answer`, which `record` accepts: the
    /// decision its harness, or plugin, applies.
    pub fn dialog_decision(self, record: &DialogRecord, answer: &Answer) -> serde_json::Value {
        match self {
            Harness::ClaudeCode => claude_code::dialog::decision(record, answer),
            // Never opens one.
            Harness::Codex => serde_json::Value::Null,
            Harness::OpenCode => opencode::dialog::decision(answer),
        }
    }

    /// Whether this agent's never-idle loop has loaded since its spawn.
    /// `None` for a harness whose loop is a native hook, which a running
    /// session cannot have failed to load.
    pub fn loop_loaded(self, project_root: &Path, scope: &str, agent: &str) -> Option<bool> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
            Harness::OpenCode => Some(
                opencode::loaded_file(project_root, scope, agent).is_ok_and(|file| file.exists()),
            ),
        }
    }

    /// The error of this agent's last turn, if it failed and no turn has
    /// succeeded since. Only a harness whose loop pm emulates reports one.
    pub fn last_turn_error(self, project_root: &Path, scope: &str, agent: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
            Harness::OpenCode => {
                let file = opencode::turn_error_file(project_root, scope, agent).ok()?;
                let error = std::fs::read_to_string(file).ok()?;
                Some(error.trim().to_string()).filter(|e| !e.is_empty())
            }
        }
    }

    /// Whether this harness would resolve its *global* copy of skill `name`
    /// over a project one, so a project custom of that name never applies.
    /// Claude Code ranks personal skills above project skills.
    pub fn project_skill_shadowed_by_global(self, home: &Path, name: &str) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::personal_skill_exists(home, name),
            Harness::Codex | Harness::OpenCode => false,
        }
    }

    /// Per-worktree files under [`config_dir`](Self::config_dir) that a
    /// feature worktree needs a copy of from main: the project's own
    /// settings and permissions, never hooks (those are user-level).
    pub fn seeded_files(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::SEEDED_FILES,
            Harness::Codex | Harness::OpenCode => &[],
        }
    }

    /// Subdirectories of the canonical store this harness projects into
    /// [`config_dir`](Self::config_dir) — and so the ones a feature
    /// worktree needs seeded from both. Empty for a harness that reads the
    /// canonical store itself.
    pub fn projected_dirs(self) -> &'static [&'static str] {
        match self {
            Harness::ClaudeCode => claude_code::PROJECTED_DIRS,
            Harness::Codex => &[],
            Harness::OpenCode => opencode::PROJECTED_DIRS,
        }
    }

    /// Whether agent definitions must be projected for this harness to see
    /// them — the precondition for a "not projected" finding.
    pub fn projects_definitions(self) -> bool {
        self.projected_dirs().contains(&"agents")
    }

    /// Whether the harness, started in `worktree`, finds `definition`: its
    /// projected copy is in the worktree's config dir or the global one.
    /// Always true for a harness that reads the canonical store itself.
    pub fn definition_projected(self, worktree: &Path, home: &Path, definition: &str) -> bool {
        if !self.projects_definitions() {
            return true;
        }
        let file = format!("{definition}.md");
        [
            Some(worktree.join(self.config_dir())),
            self.global_config_dir(home),
        ]
        .into_iter()
        .flatten()
        .any(|dir| dir.join("agents").join(&file).is_file())
    }

    /// Project the canonical asset store (`<canonical_root>/{agents,skills}`)
    /// into this harness's own layout under `target_root`, limited to
    /// `scope`. Copies over same-named files and never deletes anything in
    /// the target. In `dry_run` nothing is written; the report still says
    /// what would be.
    pub fn project_assets(
        self,
        canonical_root: &Path,
        target_root: &Path,
        scope: &ProjectionScope<'_>,
        dry_run: bool,
    ) -> Result<Projection> {
        match self {
            Harness::ClaudeCode => {
                claude_code::project_assets(canonical_root, target_root, scope, dry_run)
            }
            Harness::Codex => Ok(Projection::default()),
            Harness::OpenCode => {
                opencode::project_assets(canonical_root, target_root, scope, dry_run)
            }
        }
    }

    /// Whatever the harness needs recorded before it will run unattended in
    /// `worktree` (codex: the directory-trust entry). Idempotent; returns
    /// whether anything was written.
    pub fn trust_worktree(self, home: &Path, worktree: &Path) -> Result<bool> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => Ok(false),
            Harness::Codex => codex::trust_dir(&codex::home_dir(home), worktree),
        }
    }

    /// Whether `worktree` is already trusted; always true for a harness
    /// without a directory-trust gate.
    pub fn worktree_trusted(self, home: &Path, worktree: &Path) -> bool {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => true,
            Harness::Codex => codex::dir_trusted(&codex::home_dir(home), worktree),
        }
    }

    /// The harness's trust in the hooks of its user-level file — the gate
    /// without which codex runs no hooks, silently. Every hook reads trusted
    /// for a harness without hook trust, and when `config` has pm launch the
    /// harness with the gate bypassed. Only a [`Probe::Fresh`] asks the
    /// harness itself, which can tell a trust outdated by a changed hook.
    pub fn hook_trust(self, config: &HarnessConfig, home: &Path, probe: Probe) -> HookTrust {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => HookTrust(None),
            Harness::Codex if config.codex.bypass_hook_trust == Some(true) => HookTrust(None),
            Harness::Codex => HookTrust(Some(codex::hook_trust::HookTrust::read(
                &codex::home_dir(home),
                probe == Probe::Fresh,
            ))),
        }
    }

    /// Events in the harness's hooks file whose entries the harness would
    /// silently register nothing for.
    pub fn malformed_hook_events(self, hooks_root: &serde_json::Value) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => Vec::new(),
            Harness::Codex => codex::flat_hook_events(hooks_root),
        }
    }

    /// How a user grants the harness's hook trust, for the doctor finding.
    pub fn hook_trust_remedy(self) -> &'static str {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => "",
            Harness::Codex => codex::HOOK_TRUST_REMEDY,
        }
    }

    /// Whether pm's composed prompt reaches this harness through the
    /// SessionStart hook rather than the command line.
    pub fn injects_prompt_at_session_start(self) -> bool {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => false,
            Harness::Codex => true,
        }
    }

    /// The SessionStart hook's stdout carrying `context`; `None` for a
    /// harness that does not
    /// [inject there](Self::injects_prompt_at_session_start).
    pub fn session_start_output(self, context: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => None,
            Harness::Codex => Some(codex::session_start_output(context)),
        }
    }

    /// Whether the harness reads the macOS keychain as it starts, so a
    /// keychain that does not answer holds it before its session starts.
    pub fn reads_keychain(self) -> bool {
        match self {
            Harness::ClaudeCode => claude_code::READS_KEYCHAIN,
            Harness::Codex => codex::READS_KEYCHAIN,
            Harness::OpenCode => opencode::READS_KEYCHAIN,
        }
    }

    /// Whether `command`, a process's command line, is the harness itself:
    /// an agent's window whose pane runs no such process has a harness that
    /// exited.
    pub fn runs_as(self, command: &str, config: &HarnessConfig) -> bool {
        let binary = match self {
            Harness::ClaudeCode => claude_code::BINARY,
            Harness::Codex => codex::BINARY,
            Harness::OpenCode => opencode::binary(&config.opencode),
        };
        launched_as(command, binary)
    }

    /// The harness's name in an export's file and root directory names.
    pub fn export_tag(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
        }
    }

    /// Why the harness's session store cannot be reached on this machine,
    /// for a caller that carries sessions as a side effect and must not fail
    /// over a harness that is configured but not installed. `None` for a
    /// store pm reads as files.
    pub fn sessions_unreachable(self, config: &HarnessConfig) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
            Harness::OpenCode => opencode::sessions::unreachable(&config.opencode),
        }
    }

    /// Whether the harness has any session recorded at `dir` that a
    /// migration could act on; never true for codex, which needs none.
    pub fn has_sessions(self, store: &SessionStore<'_>, dir: &Path) -> Result<bool> {
        match self {
            Harness::ClaudeCode => Ok(claude_code::sessions::has_sessions(
                &store.claude_base(),
                dir,
            )),
            Harness::Codex => Ok(false),
            Harness::OpenCode => opencode::sessions::has_sessions(&store.config.opencode, dir),
        }
    }

    /// Make the sessions recorded at `from` resumable at `to`, which must
    /// exist. Sessions named in `in_use` have an agent running on them and
    /// are left where they are, each reported with its agent.
    pub fn migrate_sessions(
        self,
        store: &SessionStore<'_>,
        from: &Path,
        to: &Path,
        in_use: &[InUse],
    ) -> Result<Vec<String>> {
        match self {
            Harness::ClaudeCode => {
                claude_code::sessions::migrate_sessions(&store.claude_base(), from, to, in_use)
            }
            Harness::Codex => Ok(vec![codex::sessions::MIGRATE_NOTE.to_string()]),
            Harness::OpenCode => {
                opencode::sessions::migrate(&store.config.opencode, from, to, in_use)
            }
        }
    }

    /// Write the sessions recorded at each job's `dir` into its `staging`,
    /// which is created when there are any. Returns, per job, what was
    /// written, for the report; `None` when its `dir` has no sessions.
    pub fn export_sessions(
        self,
        home: &Path,
        jobs: &[ExportJob<'_>],
    ) -> Result<Vec<Option<String>>> {
        match self {
            Harness::ClaudeCode => jobs
                .iter()
                .map(|job| {
                    claude_code::sessions::export(
                        &SessionStore {
                            home,
                            config: job.config,
                        }
                        .claude_base(),
                        job.dir,
                        &job.staging,
                    )
                })
                .collect(),
            Harness::Codex => {
                let targets: Vec<(&Path, &Path)> = jobs
                    .iter()
                    .map(|job| (job.dir, job.staging.as_path()))
                    .collect();
                codex::sessions::export(&codex::home_dir(home), &targets)
            }
            Harness::OpenCode => jobs
                .iter()
                .map(|job| opencode::sessions::export(&job.config.opencode, job.dir, &job.staging))
                .collect(),
        }
    }

    /// Install what [`export_sessions`](Self::export_sessions) wrote to
    /// `staging` for the directory `from` as sessions of `to`. Never
    /// replaces a session the store already has.
    pub fn import_sessions(
        self,
        store: &SessionStore<'_>,
        staging: &Path,
        from: &Path,
        to: &Path,
    ) -> Result<ImportOutcome> {
        match self {
            Harness::ClaudeCode => {
                claude_code::sessions::import(&store.claude_base(), staging, from, to)
            }
            Harness::Codex => codex::sessions::import(&codex::home_dir(store.home), staging),
            Harness::OpenCode => opencode::sessions::import(&store.config.opencode, staging, to),
        }
    }
}

/// The longest pm waits on one call to a harness binary that answers from
/// local state, killing it after.
pub(crate) const CALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// A finding about a harness's `[harness.<name>]` settings
/// ([`Harness::config_issues`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub kind: ConfigIssueKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigIssueKind {
    /// Settings the harness or pm refuses or drops.
    Invalid,
    /// A variable a setting names is unset in pm's environment; the agent's
    /// own environment may still set it.
    KeyUnset,
    /// A provider a model or definition needs that pm's agents cannot reach.
    ProviderUnreachable,
}

/// How pm's Stop hook gets the continuation to an agent
/// ([`Harness::wake`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// It waits inside the turn and answers `block`, which the harness, or
    /// the plugin that ran it, delivers.
    Block,
    /// It runs on once the turn has ended, and exits 2 with the
    /// continuation on stderr, which wakes the session.
    Rewake,
    /// It runs on once the turn has ended, and puts the continuation on the
    /// session's queue ([`Harness::queue_prompt`]).
    Queue,
}

/// Every harness the project's agents run on: the default plus whatever
/// `[agents.harness]` resolves to for any configured agent, with the same
/// project-over-global, `""`-masks precedence as a spawn, so pm stops
/// installing for a masked harness. Unparseable values are skipped here —
/// they error at spawn, where it matters.
pub fn harnesses_in_use(project: &AgentsConfig, global: &AgentsConfig) -> Vec<Harness> {
    let mut out = vec![Harness::default()];
    for key in project.harness.keys().chain(global.harness.keys()) {
        if let Some(value) = layered(&project.harness, &global.harness, key)
            && let Ok(h) = value.parse::<Harness>()
            && !out.contains(&h)
        {
            out.push(h);
        }
    }
    out
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Harness {
    type Err = PmError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Harness::SUPPORTED
            .iter()
            .copied()
            .find(|h| h.as_str() == s)
            .ok_or_else(|| PmError::HarnessUnsupported {
                value: s.to_string(),
                supported: Harness::SUPPORTED
                    .iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }
}

/// Whether `command` runs `binary`, by file name: as the program, or as
/// the script an interpreter runs (`node …/codex`).
fn launched_as(command: &str, binary: &str) -> bool {
    let name = Path::new(binary).file_name();
    name.is_some()
        && command
            .split(' ')
            .take(2)
            .any(|word| Path::new(word).file_name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_supported_names() {
        assert_eq!(
            "claude-code".parse::<Harness>().unwrap(),
            Harness::ClaudeCode
        );
        assert_eq!(Harness::ClaudeCode.to_string(), "claude-code");
        assert_eq!(Harness::default(), Harness::ClaudeCode);
    }

    #[test]
    fn unsupported_name_errors_with_supported_list() {
        let err = "aider".parse::<Harness>().unwrap_err().to_string();
        assert_eq!(
            err,
            "harness 'aider' is not supported yet; supported: claude-code, codex, opencode"
        );
        assert_eq!("codex".parse::<Harness>().unwrap(), Harness::Codex);
        assert_eq!(Harness::Codex.to_string(), "codex");
        assert_eq!("opencode".parse::<Harness>().unwrap(), Harness::OpenCode);
        assert_eq!(Harness::OpenCode.to_string(), "opencode");
    }

    #[test]
    fn serde_uses_kebab_case_string() {
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            h: Harness,
        }
        let toml_str = toml::to_string(&Wrap {
            h: Harness::ClaudeCode,
        })
        .unwrap();
        assert_eq!(toml_str.trim(), r#"h = "claude-code""#);
        let back: Wrap = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.h, Harness::ClaudeCode);
        // The registry's spelling is the config's, for every harness.
        for h in Harness::SUPPORTED {
            let stored = toml::to_string(&Wrap { h: *h }).unwrap();
            assert_eq!(stored.trim(), format!("h = \"{h}\""));
            assert_eq!(toml::from_str::<Wrap>(&stored).unwrap().h, *h);
        }
    }

    #[test]
    fn harnesses_in_use_is_default_plus_configured() {
        let mut project = AgentsConfig::default();
        project
            .harness
            .insert("implementer".into(), "claude-code".into());
        project.harness.insert("x".into(), "aider".into());
        assert_eq!(
            harnesses_in_use(&project, &AgentsConfig::default()),
            vec![Harness::ClaudeCode]
        );
        project.harness.insert("main".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&project, &AgentsConfig::default()),
            vec![Harness::ClaudeCode, Harness::Codex]
        );
    }

    #[test]
    fn harnesses_in_use_counts_a_wildcard_row() {
        let mut global = AgentsConfig::default();
        global.harness.insert("*".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&AgentsConfig::default(), &global),
            vec![Harness::ClaudeCode, Harness::Codex]
        );
        // A project wildcard masks the global one for every agent.
        let mut project = AgentsConfig::default();
        project.harness.insert("*".into(), String::new());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode]
        );
    }

    #[test]
    fn harnesses_in_use_applies_project_over_global_precedence() {
        let mut global = AgentsConfig::default();
        global.harness.insert("main".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&AgentsConfig::default(), &global),
            vec![Harness::ClaudeCode, Harness::Codex]
        );

        // The project masks the global row: codex is no longer in use.
        let mut project = AgentsConfig::default();
        project.harness.insert("main".into(), String::new());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode]
        );
        // …or overrides it outright.
        project.harness.insert("main".into(), "claude-code".into());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode]
        );
        // A global row for another agent still counts.
        global.harness.insert("reviewer".into(), "codex".into());
        assert_eq!(
            harnesses_in_use(&project, &global),
            vec![Harness::ClaudeCode, Harness::Codex]
        );
    }
}
