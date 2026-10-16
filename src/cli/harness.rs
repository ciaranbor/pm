use clap::Subcommand;
use std::path::PathBuf;

use pm::harness::Harness;

#[derive(Subcommand)]
pub enum HarnessCommands {
    /// Lifecycle hooks managed by pm
    #[command(subcommand)]
    Hooks(HarnessHooksCommands),
    /// Manage bundled skills (global `~/.agents/skills/`, projected per harness)
    #[command(subcommand)]
    Skills(HarnessSkillsCommands),
    /// Manage bundled agent definitions (global `~/.agents/agents/`, projected per harness)
    #[command(subcommand)]
    Agents(HarnessAgentsCommands),
    /// Per-feature harness settings files (permissions etc.)
    Settings {
        /// Harness whose settings to manage
        #[arg(long, global = true, default_value = "claude-code")]
        harness: Harness,
        #[command(subcommand)]
        command: HarnessSettingsCommands,
    },
    /// Pull main's custom skills, agent definitions and settings into a feature
    /// and project its own skills (never overwrites a file the feature's branch tracks)
    Pull {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Preview the files that would be written without writing them
        #[arg(long)]
        dry_run: bool,
    },
    /// Make harness sessions recorded at an old path resumable in the current directory
    Migrate {
        /// The old absolute path where the project previously lived
        #[arg(long)]
        from: PathBuf,
        /// Harness whose sessions to migrate
        #[arg(long, default_value = "claude-code")]
        harness: Harness,
    },
    /// Export the sessions of a project's main and feature worktrees for transfer to another machine
    Export {
        /// Export sessions for all registered projects (default: current project only)
        #[arg(long)]
        all: bool,
        /// With --all, a registered project to export (repeatable; default: every one)
        #[arg(long = "project", value_name = "NAME", requires = "all")]
        projects: Vec<String>,
        /// Output tarball path (default: pm-<harness>-<name>.tar.gz in current directory,
        /// `pm-claude-…` for claude-code)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Harness whose sessions to export
        #[arg(long, default_value = "claude-code")]
        harness: Harness,
    },
    /// Import harness sessions from an exported tarball (after `pm restore`, so feature worktrees exist)
    Import {
        /// Path to the tarball created by `pm harness export`
        tarball: PathBuf,
        /// Harness whose sessions to import (default: the one that exported them)
        #[arg(long)]
        harness: Option<Harness>,
    },
    /// List the harnesses pm can spawn
    List,
    /// Check the installed harness binary for the capabilities pm relies on
    Probe {
        /// Harness to probe
        #[arg(long, default_value = "claude-code")]
        harness: Harness,
    },
    /// Run a codex agent's app-server and TUI (what its window runs; not for direct use)
    #[command(hide = true)]
    CodexHost {
        /// Approval policy for the agent's turns
        #[arg(long)]
        approval: String,
        /// Sandbox mode for the agent's turns
        #[arg(long)]
        sandbox: String,
        /// A directory outside the worktree the agent must be able to write (repeatable)
        #[arg(long)]
        writable: Vec<PathBuf>,
        /// Model id
        #[arg(long)]
        model: Option<String>,
        /// Session to resume
        #[arg(long, conflicts_with = "fork")]
        resume: Option<String>,
        /// Session to fork and open the fork of
        #[arg(long)]
        fork: Option<String>,
        /// The launch prompt
        #[arg(long, allow_hyphen_values = true)]
        prompt: Option<String>,
        /// What codex shows the session as
        #[arg(long)]
        name: Option<String>,
    },
    /// Run an opencode agent's server and TUI (what its window runs; not for direct use)
    #[command(hide = true)]
    OpencodeHost {
        /// The opencode binary
        #[arg(long)]
        binary: String,
        /// Approve whatever no permission rule denies
        #[arg(long)]
        auto: bool,
        /// Session to open
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum HarnessAgentsCommands {
    /// List bundled agent definitions, their global install status, and any project override
    List,
    /// Uninstall bundled agent definitions from ~/.agents/agents/ (and its projections)
    ///
    /// `pm update` reinstalls them; `[bundled.disable]` in the global config keeps them off.
    Uninstall {
        /// Agent name (required unless --all is passed)
        name: Option<String>,
        /// Uninstall all bundled agent definitions
        #[arg(long)]
        all: bool,
    },
    /// Install bundled agent definitions into ~/.agents/agents/ (projected into ~/.claude/agents/)
    Install {
        /// Agent name (installs all if omitted)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum HarnessSettingsCommands {
    /// List a scope's settings files
    List {
        /// Scope: `main` or a feature (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Push feature's settings to main
    Push {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Pull main's settings into a feature
    Pull {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Show differences between main and feature settings
    Diff {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Merge feature and main settings (union), writing result to main
    Merge {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// On scalar conflicts, let the feature (ours) win instead of main (theirs)
        #[arg(long)]
        ours: bool,
    },
}

#[derive(Subcommand)]
pub enum HarnessSkillsCommands {
    /// List bundled skills, their global install status, and any project override
    List,
    /// Install bundled skills into ~/.agents/skills/ (projected into ~/.claude/skills/)
    Install {
        /// Skill name (installs all if omitted)
        name: Option<String>,
    },
    /// Uninstall bundled skills from ~/.agents/skills/ (and its projections)
    ///
    /// `pm update` reinstalls them; `[bundled.disable]` in the global config keeps them off.
    Uninstall {
        /// Skill name (required unless --all is passed)
        name: Option<String>,
        /// Uninstall all bundled skills
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum HarnessHooksCommands {
    /// Install pm hooks into each harness's user-level hooks file, moving any out of project files
    Install,
    /// Stop hook handler — called by the harness on every Stop event (not for direct use)
    Stop {
        /// The harness whose hook entry runs it once the turn has ended;
        /// omitted, it blocks inside the turn (opencode's plugin)
        harness: Option<pm::harness::Harness>,
    },
    /// SessionStart hook handler — called by the harness on session start (not for direct use)
    SessionStart,
    /// SessionEnd hook handler — reads the agent dead once its harness exits (not for direct use)
    SessionEnd {
        /// The harness whose hook payload arrives on stdin
        harness: pm::harness::Harness,
    },
    /// UserPromptSubmit hook handler — unblocks a feature on the user's input (not for direct use)
    UserPrompt,
    /// UserPromptSubmit waiter — wakes an interrupted turn for a message sent during it (not for direct use)
    TurnWaiter {
        /// The harness whose hook entry runs it in the background
        harness: pm::harness::Harness,
    },
    /// Status hook handler — records when an agent waits on the user (not for direct use)
    Waiting {
        /// The harness whose hook payload arrives on stdin
        harness: pm::harness::Harness,
    },
    /// Dialog hook handler — waits for a dialog's answer from `pm serve` (not for direct use)
    Dialog {
        /// The harness whose dialog payload arrives on stdin
        harness: pm::harness::Harness,
    },
    /// Guard hook handler — refuses an edit to a directory the agent's scope must not edit (not for direct use)
    Guard {
        /// The harness whose tool-call payload arrives on stdin
        harness: pm::harness::Harness,
    },
    /// Restart an agent `pm update` deferred, now idle (started by the Stop hook)
    #[command(hide = true)]
    RestartAtIdle {
        agent: String,
        #[arg(long)]
        scope: String,
    },
}
