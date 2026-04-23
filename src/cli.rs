use clap::{ArgGroup, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "pm",
    about = "Terminal-based project manager built around tmux and git worktrees"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new pm project with a git repo
    Init {
        /// Path for the new project root
        path: PathBuf,
        /// Clone a remote repo instead of running git init
        #[arg(long)]
        git: Option<String>,
    },
    /// Register an existing git repo as a pm project
    Register {
        /// Path to the existing git repo
        path: PathBuf,
        /// Custom project name (defaults to directory name)
        #[arg(long)]
        name: Option<String>,
        /// Move the repo into the wrapper instead of symlinking
        #[arg(long, rename_all = "kebab-case")]
        r#move: bool,
    },
    /// List all registered projects
    List,
    /// Open/reconstruct tmux sessions for the current project
    Open,
    /// Feature management
    #[command(subcommand)]
    Feat(FeatCommands),
    /// Agent management (spawn, list)
    #[command(subcommand)]
    Agent(AgentCommands),
    /// Inter-agent messaging (send, read, next, list, wait)
    #[command(subcommand)]
    Msg(MsgCommands),
    /// Claude Code settings, skills, and session management
    #[command(subcommand)]
    Claude(ClaudeCommands),
    /// Close all tmux sessions for the current project (counterpart to `pm open`)
    Close,
    /// Delete a project (teardown features, sessions, state, and registry entry)
    Delete {
        /// Project name (defaults to current project from CWD)
        #[arg(long)]
        project: Option<String>,
        /// Skip safety checks and force-remove worktree directories
        #[arg(long)]
        force: bool,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Show project dashboard (features, PR status, health)
    Status {
        /// Project name (defaults to current project from CWD)
        #[arg(long)]
        project: Option<String>,
    },
    /// Diagnose project health and detect state drift
    Doctor {
        /// Auto-fix clear-cut issues (orphaned state, stuck initializing, missing tmux sessions)
        #[arg(long)]
        fix: bool,
        /// Project name (defaults to current project from CWD)
        #[arg(long)]
        project: Option<String>,
    },
    /// Reinstall bundled assets (hooks, skills, agents) to projects
    Upgrade {
        /// Upgrade all registered projects instead of just the current one
        #[arg(long)]
        all: bool,
    },
    /// Restore all projects on a fresh machine from the global registry
    Restore,
    /// Git-backed state management (.pm/ backup and sync)
    #[command(subcommand)]
    State(StateCommands),
    /// Write a summary doc from a feature worktree
    Summary {
        #[command(subcommand)]
        command: SummaryCommands,
    },
    /// Generate shell completion scripts
    #[command(hide = true)]
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[derive(Subcommand)]
pub enum StateCommands {
    /// Initialise git repo in .pm/ (or ~/.config/pm/ with --global) for state backup and sync
    Init {
        /// Operate on the global registry (~/.config/pm/) instead of the project .pm/
        #[arg(long)]
        global: bool,
        /// Set remote URL and pull after initialising (combines init + remote + pull)
        #[arg(long)]
        remote: Option<String>,
    },
    /// Set the git remote for the state repo (interactive if no URL given)
    Remote {
        /// Remote URL (e.g. a bare git repo or GitHub URL). Omit for interactive setup.
        url: Option<String>,
        /// Operate on the global registry (~/.config/pm/) instead of the project .pm/
        #[arg(long)]
        global: bool,
    },
    /// Auto-commit and push state to the remote
    Push {
        /// Operate on the global registry (~/.config/pm/) instead of the project .pm/
        #[arg(long)]
        global: bool,
    },
    /// Pull state from the remote
    Pull {
        /// Operate on the global registry (~/.config/pm/) instead of the project .pm/
        #[arg(long)]
        global: bool,
    },
    /// Show git status of the state repo
    Status {
        /// Operate on the global registry (~/.config/pm/) instead of the project .pm/
        #[arg(long)]
        global: bool,
    },
    /// Backfill repo_url and state_remote in global registry from existing projects
    Backfill,
}

#[derive(Subcommand)]
pub enum SummaryCommands {
    /// Write (or overwrite) the summary doc for the current feature
    Write {
        /// Content string or path to a file
        content: String,
    },
}

#[derive(Subcommand)]
pub enum ClaudeCommands {
    /// Per-feature Claude Code settings
    #[command(subcommand)]
    Settings(ClaudeSettingsCommands),
    /// Manage bundled Claude Code skills
    #[command(subcommand)]
    Skills(ClaudeSkillsCommands),
    /// Manage bundled Claude Code agent definitions
    #[command(subcommand)]
    Agents(ClaudeAgentsCommands),
    /// Claude Code lifecycle hooks managed by pm
    #[command(subcommand)]
    Hooks(HooksCommands),
    /// Migrate Claude Code sessions from an old project path to the current directory
    Migrate {
        /// The old absolute path where the project previously lived
        #[arg(long)]
        from: PathBuf,
    },
    /// Export Claude Code sessions for transfer to another machine
    Export {
        /// Export sessions for all registered projects (default: current project only)
        #[arg(long)]
        all: bool,
        /// Output tarball path (default: pm-claude-<name>.tar.gz in current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import Claude Code sessions from an exported tarball
    Import {
        /// Path to the tarball created by `pm claude export`
        tarball: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ClaudeAgentsCommands {
    /// List available bundled agent definitions and their install status
    List,
    /// Uninstall bundled agent definitions (project-level by default, or --global)
    Uninstall {
        /// Agent name (required unless --all is passed)
        name: Option<String>,
        /// Uninstall all bundled agent definitions
        #[arg(long)]
        all: bool,
        /// Uninstall from ~/.claude/agents/ instead of the project
        #[arg(long)]
        global: bool,
    },
    /// Install bundled agent definitions (project-level by default, or --global for ~/.claude/agents/)
    Install {
        /// Agent name (installs all if omitted)
        name: Option<String>,
        /// Install to ~/.claude/agents/ instead of the project
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
pub enum ClaudeSettingsCommands {
    /// List Claude Code settings (main or feature)
    List {
        /// Feature name (detected from CWD if omitted; works from main worktree too)
        name: Option<String>,
    },
    /// Push feature's .claude/ settings to main
    Push {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Pull main's .claude/ settings into a feature
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
pub enum ClaudeSkillsCommands {
    /// List available bundled skills and their install status
    List,
    /// Install bundled skills (project-level by default, or --global for ~/.claude/skills/)
    Install {
        /// Skill name (installs all if omitted)
        name: Option<String>,
        /// Install to ~/.claude/skills/ instead of the project
        #[arg(long)]
        global: bool,
    },
    /// Uninstall bundled skills (project-level by default, or --global)
    Uninstall {
        /// Skill name (required unless --all is passed)
        name: Option<String>,
        /// Uninstall all bundled skills
        #[arg(long)]
        all: bool,
        /// Uninstall from ~/.claude/skills/ instead of the project
        #[arg(long)]
        global: bool,
    },
    /// Pull skills from main into a feature's .claude/skills/
    Pull {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum HooksCommands {
    /// Install pm hooks (Stop + SessionStart) into main/.claude/settings.json
    Install,
    /// Stop hook handler — called by Claude Code on every Stop event (not for direct use)
    Stop,
    /// SessionStart hook handler — called by Claude Code on session start (not for direct use)
    SessionStart,
}

#[derive(Subcommand)]
pub enum AgentCommands {
    /// Spawn an agent in a tmux window
    Spawn {
        /// Agent name (omit to respawn all previously active agents)
        name: Option<String>,
        /// Initial context for the agent
        #[arg(long)]
        context: Option<String>,
        /// Enable acceptEdits permission mode
        #[arg(long)]
        edit: bool,
    },
    /// List agents in the current feature
    List {
        /// Only show active agents
        #[arg(long)]
        active: bool,
    },
    /// Stop a running agent (kill window, mark inactive)
    Stop {
        /// Agent name
        name: String,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Send a checklist to an agent for self-verification (omit name to check all active agents)
    Check {
        /// Agent name (omit to check all active agents)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum MsgCommands {
    /// Send a message to an agent's inbox
    Send {
        /// Recipient agent name
        agent: String,
        /// Message body (if omitted, reads from stdin)
        message: Option<String>,
        /// Sender identity (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
        /// Deliver to a different scope (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
        /// Deliver to the parent scope (base branch's feature). Shorthand for --scope <base>.
        #[arg(long, conflicts_with = "scope")]
        upstream: bool,
        /// Deliver to a different project (by registered name). Resolves the
        /// target project's root from ~/.config/pm/projects/<name>.toml and
        /// delivers there. Auto-spawn is disabled for cross-project messages.
        #[arg(long, conflicts_with = "upstream")]
        project: Option<String>,
    },
    /// Read the next unread message and advance the cursor
    Read {
        /// Which sender's queue to read from. Required with --index.
        /// Without --index, inferred when exactly one sender has unread messages.
        #[arg(long)]
        from: Option<String>,
        /// Re-read a specific message without advancing the cursor.
        /// Absolute index ("3"), or relative to the cursor ("+2", "-1").
        /// If omitted, reads the next unread message and advances.
        #[arg(long, value_name = "SPEC")]
        index: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
        /// Read from a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
    },
    /// List all messages in your inbox, with cursor position markers
    List {
        /// Only show messages from this sender
        #[arg(long)]
        from: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
        /// List messages from a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
    },
    /// Block until a message arrives in your inbox
    Wait {
        /// Only block on messages from this sender
        #[arg(long)]
        from: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
        /// Wait on a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum FeatCommands {
    /// Create a new feature (branch + worktree + tmux session)
    New {
        /// Branch name (slashes are sanitized to dashes for the feature name)
        name: String,
        /// Override the derived feature name
        #[arg(long)]
        feature_name: Option<String>,
        /// Initial context (literal text or path to a file)
        #[arg(long)]
        context: Option<String>,
        /// Base branch to stack on (defaults to current branch from CWD)
        #[arg(long)]
        base: Option<String>,
        /// Force --permission-mode acceptEdits on the spawned Claude session
        #[arg(long)]
        edit: bool,
        /// Agent to spawn (overrides project default)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Adopt an existing branch as a feature (worktree + tmux session)
    Adopt {
        /// Branch name to adopt (slashes are sanitized to dashes for the feature name)
        name: String,
        /// Override the derived feature name
        #[arg(long)]
        feature_name: Option<String>,
        /// Initial context (literal text or path to a file)
        #[arg(long)]
        context: Option<String>,
        /// Migrate Claude Code sessions from this old path
        #[arg(long)]
        from: Option<PathBuf>,
        /// Force --permission-mode acceptEdits on the spawned Claude session
        #[arg(long)]
        edit: bool,
        /// Agent to spawn (overrides project default)
        #[arg(long)]
        agent: Option<String>,
    },
    /// List all features with their status
    List,
    /// Show detailed info for a feature
    Info {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Switch to a feature's tmux session
    Switch {
        /// Feature name (omit for interactive picker)
        name: Option<String>,
    },
    /// Delete a feature (with safety checks)
    Delete {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Skip safety checks
        #[arg(long)]
        force: bool,
    },
    /// Merge a feature branch into the base branch
    Merge {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Keep the feature after merge (preserve session, worktree, branch, and state)
        #[arg(long)]
        keep: bool,
    },
    /// GitHub PR management (create, edit)
    #[command(subcommand)]
    Pr(PrCommands),
    /// Mark a feature's PR as ready for review
    Ready {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Rename a feature (branch, worktree, tmux session, state)
    Rename {
        /// New feature name
        new_name: String,
        /// Current feature name (detected from CWD if omitted)
        #[arg(long)]
        old_name: Option<String>,
    },
    /// Check out a PR for review (fetch branch + worktree + tmux session)
    Review {
        /// PR number or GitHub PR URL
        pr: String,
    },
    /// Sync feature statuses with their linked GitHub PRs
    Sync {
        /// Feature name (syncs just this feature). Detected from CWD if omitted. If not in a feature worktree, syncs all features.
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PrCommands {
    /// Create or link a GitHub PR for a feature
    Create {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Create a non-draft (ready) PR instead of draft
        #[arg(long)]
        ready: bool,
        /// PR body (literal text or path to a file; overrides template)
        #[arg(long)]
        body: Option<String>,
    },
    /// Edit an existing PR's title and/or description
    #[command(group(ArgGroup::new("edit_fields").required(true).multiple(true).args(["title", "body"])))]
    Edit {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// New PR title
        #[arg(long)]
        title: Option<String>,
        /// New PR body (literal text or path to a file)
        #[arg(long)]
        body: Option<String>,
    },
}
