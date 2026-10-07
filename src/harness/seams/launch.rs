//! Launch seams: the command line that starts an agent, what must precede
//! it, and whether the installed binary can run one at all.

use std::path::Path;

use crate::error::Result;
use crate::harness::{
    Harness, LaunchContext, PreLaunch, Probe, SpawnSpec, claude_code, codex, opencode,
};
use crate::state::project::HarnessConfig;

impl Harness {
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

    /// How the composed prompt reaches an agent, for probe and doctor
    /// messages.
    pub fn prompt_mechanism(self) -> String {
        match self {
            Harness::ClaudeCode => claude_code::PROMPT_MECHANISM.to_string(),
            Harness::Codex => codex::prompt_mechanism(),
            Harness::OpenCode => opencode::prompt_mechanism(),
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

    /// The manual step that gives a new machine this harness's credentials,
    /// which no pm export carries.
    pub fn credentials_step(self, config: &HarnessConfig) -> String {
        match self {
            Harness::ClaudeCode => claude_code::CREDENTIALS_STEP.to_string(),
            Harness::Codex => codex::CREDENTIALS_STEP.to_string(),
            Harness::OpenCode => opencode::credentials_step(&config.opencode),
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
