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
mod opencode;
mod probe;
mod screen;
mod transcript;

pub use probe::Probe;

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{PmError, Result};
use crate::fs_utils;
use crate::state::project::{AgentsConfig, HarnessConfig, layered};
use crate::state::runtime::{Waiting, WaitingClass};

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
    /// agents started in `worktree`, where only the harness can tell.
    pub fn config_issues(self, config: &HarnessConfig, worktree: &Path) -> Vec<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => Vec::new(),
            Harness::OpenCode => opencode::config_issues(&config.opencode, worktree),
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
    /// it can't tell. `home` holds the harness's config. opencode's never
    /// stays unarmed — its plugin waits again after any turn and stops only
    /// on purpose — so its screen is never read.
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
            Harness::OpenCode => None,
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

    /// Why this agent's never-idle loop stopped itself, if it did. Only a
    /// harness whose loop pm emulates can report one.
    pub fn loop_stopped(self, project_root: &Path, scope: &str, agent: &str) -> Option<String> {
        match self {
            Harness::ClaudeCode | Harness::Codex => None,
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

    /// Whether the harness has recorded trust for the hook at `hooks.<event>[entry].hooks[hook]`
    /// of its user-level file — the gate without which codex runs no hooks,
    /// silently. Always true for a harness without hook trust, and when
    /// `config` has pm launch the harness with the gate bypassed.
    pub fn hook_trusted(
        self,
        config: &HarnessConfig,
        home: &Path,
        event: &str,
        entry: usize,
        hook: usize,
    ) -> bool {
        match self {
            Harness::ClaudeCode | Harness::OpenCode => true,
            Harness::Codex => {
                if config.codex.bypass_hook_trust == Some(true) {
                    return true;
                }
                let codex_home = codex::home_dir(home);
                let key =
                    codex::hook_trust_key(&codex_home.join(codex::HOOKS_FILE), event, entry, hook);
                codex::hook_trusted(&codex_home, &key)
            }
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

/// How the session seams reach a harness's store.
#[derive(Debug, Clone, Copy)]
pub struct SessionStore<'a> {
    /// The user's home; a harness with a file store derives it from here.
    pub home: &'a Path,
    /// The `[harness.*]` settings in effect where the sessions belong.
    pub config: &'a HarnessConfig,
}

impl SessionStore<'_> {
    fn claude_base(&self) -> PathBuf {
        self.home.join(claude_code::CONFIG_DIR)
    }
}

/// One directory whose sessions [`Harness::export_sessions`] writes.
pub struct ExportJob<'a> {
    /// The `[harness.*]` settings in effect where the sessions belong.
    pub config: &'a HarnessConfig,
    /// The directory the sessions were recorded at.
    pub dir: &'a Path,
    /// Where they are written.
    pub staging: PathBuf,
}

/// A session an agent is running on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InUse {
    pub session_id: String,
    /// `<scope>/<agent>`, for the report.
    pub agent: String,
}

/// What importing one directory's sessions did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Nothing was written, and why.
    Skipped(String),
    Imported {
        /// What was imported, for the report.
        detail: String,
        notes: Vec<String>,
    },
}

/// `imported` of `total` sessions were new to the store; the rest were
/// already there and left untouched.
pub(crate) fn per_session_outcome(imported: usize, total: usize) -> ImportOutcome {
    if total == 0 {
        return ImportOutcome::Skipped("no sessions in the export".to_string());
    }
    if imported == 0 {
        return ImportOutcome::Skipped(format!("all {total} session(s) already exist locally"));
    }
    let detail = match total - imported {
        0 => format!("{imported} session(s)"),
        present => format!("{imported} session(s), {present} already present"),
    };
    ImportOutcome::Imported {
        detail,
        notes: Vec::new(),
    }
}

/// What a projection wrote (or, in dry-run, would write), as paths
/// relative to the target root.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub written: Vec<PathBuf>,
    /// The subset of `written` that replaced an existing file with
    /// different content.
    pub replaced: Vec<PathBuf>,
}

impl Projection {
    pub fn is_empty(&self) -> bool {
        self.written.is_empty()
    }
}

/// Which part of a projection [`Harness::project_assets`] writes.
#[derive(Debug, Default)]
pub struct ProjectionScope<'a> {
    /// The projected subdirs to write; `None` for all of the harness's.
    pub subdirs: Option<&'a [&'a str]>,
    /// Target paths, relative to the target root, never written (a
    /// directory: its whole subtree).
    pub keep: HashSet<PathBuf>,
}

/// Copy `<canonical_root>/<subdir>` over `<target_root>/<subdir>` for each
/// `subdirs` entry within `scope`, recording what changed. Shared by every
/// harness whose projection is a plain copy.
pub(crate) fn project_by_copy(
    canonical_root: &Path,
    target_root: &Path,
    subdirs: &[&str],
    scope: &ProjectionScope<'_>,
    dry_run: bool,
) -> Result<Projection> {
    let mut out = Projection::default();
    for sub in subdirs {
        if scope.subdirs.is_some_and(|only| !only.contains(sub)) {
            continue;
        }
        let src = canonical_root.join(sub);
        if !src.is_dir() {
            continue;
        }
        let dst = target_root.join(sub);
        let keep: HashSet<PathBuf> = scope
            .keep
            .iter()
            .filter_map(|p| p.strip_prefix(sub).ok().map(Path::to_path_buf))
            .collect();
        for (rel, replaced) in fs_utils::sync_tree_except(&src, &dst, &keep, dry_run)? {
            let rel = Path::new(sub).join(rel);
            if replaced {
                out.replaced.push(rel.clone());
            }
            out.written.push(rel);
        }
    }
    Ok(out)
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

/// A harness-neutral description of the agent session pm wants to launch.
/// Every field left unset omits the corresponding flag, so the agent
/// inherits the harness's own default for that setting.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnSpec<'a> {
    /// Agent definition to launch; `None` is a plain, definition-less session.
    pub definition: Option<&'a str>,
    /// The composed baseline + notice-board file appended to the system prompt.
    pub append_prompt_file: Option<&'a str>,
    /// Initial positional prompt (or the never-idle sentinel).
    pub prompt: Option<&'a str>,
    /// Session id to resume.
    pub resume_session: Option<&'a str>,
    /// With `resume_session`, load its transcript under a fresh session id.
    pub fork_session: bool,
    /// Harness-specific, like `model`: whatever the config row said.
    pub permission_mode: Option<&'a str>,
    pub model: Option<&'a str>,
    /// Directories outside the worktree the agent must be able to write
    /// (pm's state, the shared `.git`) — what a sandboxing harness opens up.
    pub writable_dirs: &'a [PathBuf],
    /// Directories outside the worktree the agent edits with its own file
    /// tools (the feature summary's), for a harness that gates those edits
    /// by directory. Each lies within a `writable_dirs` entry.
    pub edit_dirs: &'a [PathBuf],
}

/// Where a spawn happens, for [`Harness::pre_launch`].
#[derive(Debug, Clone, Copy)]
pub struct LaunchContext<'a> {
    pub project_root: &'a Path,
    /// The scope the agent belongs to.
    pub feature: &'a str,
    pub worktree: &'a Path,
    /// The agent's display name.
    pub agent: &'a str,
}

/// What [`Harness::pre_launch`] prepared for the command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PreLaunch {
    /// The session the harness will open, known before it starts — recorded
    /// on the registry entry at spawn. `None` for a harness that reports its
    /// session through the SessionStart hook.
    pub session_id: Option<String>,
    /// Environment the command must run with.
    pub env: Vec<(String, String)>,
    /// Variables the command must not inherit from the window's shell.
    pub env_remove: Vec<String>,
    /// Remarks for the spawn line.
    pub notes: Vec<String>,
}

/// A session id is bound to the harness that produced it. Returns the id
/// to resume only when `stored` (the harness recorded on the registry
/// entry) still matches `resolved` (what config says now); otherwise the
/// agent must start fresh.
pub fn resumable_session(session_id: &str, stored: Harness, resolved: Harness) -> Option<String> {
    if session_id.is_empty() || stored != resolved {
        return None;
    }
    Some(session_id.to_string())
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
    fn claude_code_build_cmd_full_spec() {
        // Every field set at once, pinning flag order through the seam.
        let dirs = vec![PathBuf::from("/proj/.pm")];
        let edit = vec![PathBuf::from("/proj/.pm/summaries")];
        let cmd = Harness::ClaudeCode.build_cmd(
            &SpawnSpec {
                definition: Some("reviewer"),
                append_prompt_file: Some("/proj/main/.agents/pm-baseline.md"),
                prompt: Some("Stand by."),
                resume_session: Some("abc123"),
                fork_session: true,
                permission_mode: Some("acceptEdits"),
                model: Some("opus"),
                writable_dirs: &dirs,
                edit_dirs: &edit,
            },
            &HarnessConfig::default(),
            &PreLaunch::default(),
        );
        assert_eq!(
            cmd,
            "claude --agent reviewer --model 'opus' \
             --append-system-prompt-file '/proj/main/.agents/pm-baseline.md' \
             --permission-mode 'acceptEdits' --add-dir='/proj/.pm/summaries' \
             --resume abc123 --fork-session 'Stand by.'"
        );
    }

    #[test]
    fn project_assets_copies_overwrites_and_never_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(".agents");
        let target = tmp.path().join(".claude");
        std::fs::create_dir_all(canonical.join("agents")).unwrap();
        std::fs::create_dir_all(canonical.join("skills/pm")).unwrap();
        std::fs::write(canonical.join("agents/reviewer.md"), "new").unwrap();
        std::fs::write(canonical.join("skills/pm/SKILL.md"), "skill").unwrap();
        std::fs::create_dir_all(target.join("agents")).unwrap();
        std::fs::write(target.join("agents/reviewer.md"), "old").unwrap();
        std::fs::write(target.join("agents/custom.md"), "mine").unwrap();

        // Dry run: reports, writes nothing.
        let dry = Harness::ClaudeCode
            .project_assets(&canonical, &target, &ProjectionScope::default(), true)
            .unwrap();
        assert_eq!(dry.written.len(), 2);
        assert_eq!(dry.replaced, vec![PathBuf::from("agents/reviewer.md")]);
        assert_eq!(
            std::fs::read_to_string(target.join("agents/reviewer.md")).unwrap(),
            "old"
        );
        assert!(!target.join("skills").exists());

        let real = Harness::ClaudeCode
            .project_assets(&canonical, &target, &ProjectionScope::default(), false)
            .unwrap();
        assert_eq!(real, dry);
        assert_eq!(
            std::fs::read_to_string(target.join("agents/reviewer.md")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("skills/pm/SKILL.md")).unwrap(),
            "skill"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("agents/custom.md")).unwrap(),
            "mine"
        );

        // In sync: nothing to do.
        let again = Harness::ClaudeCode
            .project_assets(&canonical, &target, &ProjectionScope::default(), true)
            .unwrap();
        assert!(again.is_empty());
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

    #[test]
    fn codex_layout_needs_no_projection_and_hooks_live_in_codex_home() {
        let home = Path::new("/h");
        assert_eq!(
            Harness::Codex.user_settings_file(home),
            Some(PathBuf::from("/h/.codex/hooks.json"))
        );
        assert_eq!(
            Harness::Codex.global_config_dir(home),
            Some(PathBuf::from("/h/.codex"))
        );
        assert!(Harness::Codex.projected_dirs().is_empty());

        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join(".agents");
        std::fs::create_dir_all(canonical.join("agents")).unwrap();
        std::fs::write(canonical.join("agents/reviewer.md"), "x").unwrap();
        let target = tmp.path().join(".codex");
        assert!(
            Harness::Codex
                .project_assets(&canonical, &target, &ProjectionScope::default(), false)
                .unwrap()
                .is_empty()
        );
        assert!(!target.exists());
    }

    #[test]
    fn trust_seams_are_inert_for_claude_code_and_gate_codex() {
        let home = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let config = HarnessConfig::default();
        assert!(Harness::ClaudeCode.worktree_trusted(home.path(), wt.path()));
        assert!(
            !Harness::ClaudeCode
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        assert!(Harness::ClaudeCode.hook_trusted(&config, home.path(), "Stop", 0, 0));

        assert!(!Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(
            Harness::Codex
                .trust_worktree(home.path(), wt.path())
                .unwrap()
        );
        assert!(Harness::Codex.worktree_trusted(home.path(), wt.path()));
        assert!(!Harness::Codex.hook_trusted(&config, home.path(), "Stop", 0, 0));
        let hooks = home.path().join(".codex/hooks.json");
        std::fs::write(
            home.path().join(".codex/config.toml"),
            format!(
                "[hooks.state.\"{}:stop:1:0\"]\ntrusted_hash = \"sha256:x\"\n",
                hooks.display()
            ),
        )
        .unwrap();
        assert!(Harness::Codex.hook_trusted(&config, home.path(), "Stop", 1, 0));
        assert!(!Harness::Codex.hook_trusted(&config, home.path(), "Stop", 0, 0));
        assert!(!Harness::Codex.hook_trusted(&config, home.path(), "SessionStart", 1, 0));

        let mut bypassed = HarnessConfig::default();
        bypassed.codex.bypass_hook_trust = Some(true);
        assert!(Harness::Codex.hook_trusted(&bypassed, home.path(), "SessionStart", 1, 0));
    }

    #[test]
    fn resumable_session_needs_an_id_and_the_same_harness() {
        assert_eq!(
            resumable_session("sess", Harness::ClaudeCode, Harness::ClaudeCode).as_deref(),
            Some("sess")
        );
        assert_eq!(
            resumable_session("sess", Harness::Codex, Harness::Codex).as_deref(),
            Some("sess")
        );
        assert_eq!(
            resumable_session("", Harness::ClaudeCode, Harness::ClaudeCode),
            None
        );
        assert_eq!(
            resumable_session("sess", Harness::ClaudeCode, Harness::Codex),
            None
        );
    }
}
