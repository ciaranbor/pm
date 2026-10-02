use clap::{ArgGroup, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

use pm::harness::Harness;

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
    /// Open/reconstruct a project's tmux sessions
    Open {
        /// Project name (defaults to current project from CWD)
        #[arg(long, conflicts_with = "all")]
        project: Option<String>,
        /// Open every registered project (e.g. after a reboot); attaches only
        /// to the current project's main session, if run from inside one
        #[arg(long)]
        all: bool,
    },
    /// Feature management
    #[command(subcommand)]
    Feat(FeatCommands),
    /// Agent management
    #[command(subcommand)]
    Agent(AgentCommands),
    /// Inter-agent messaging
    #[command(subcommand)]
    Msg(MsgCommands),
    /// Agent harness integration
    #[command(subcommand)]
    Harness(HarnessCommands),
    /// Hidden alias for `pm harness` kept for one release
    #[command(subcommand, hide = true)]
    Claude(HarnessCommands),
    /// Close all tmux sessions for the current project (counterpart to `pm open`)
    Close {
        /// Close every registered project's sessions, not just the current one
        #[arg(long)]
        all: bool,
    },
    /// Delete a project (teardown features, sessions, state, and registry entry; worktrees stay on disk)
    Delete {
        /// Project name (defaults to current project from CWD)
        #[arg(long)]
        project: Option<String>,
        /// Skip safety checks and remove the worktrees from disk, `main` and its repository included
        #[arg(long)]
        force: bool,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Show project dashboard (what each feature needs, health)
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
    /// Reinstall pm's hooks and bundled assets
    Upgrade {
        /// Upgrade all registered projects instead of just the current one
        #[arg(long)]
        all: bool,
        /// Preview changes without writing anything
        #[arg(long, alias = "check")]
        dry_run: bool,
    },
    /// Restore all projects on a fresh machine from the global registry
    Restore,
    /// Pull latest pm source, rebuild, and upgrade all projects
    SelfUpdate,
    /// Git-backed state management (.pm/ backup and sync)
    #[command(subcommand)]
    State(StateCommands),
    /// Per-feature workflow management
    #[command(subcommand)]
    Workflow(WorkflowCommands),
    /// tmux integration: pm state published on the tmux server
    #[command(subcommand)]
    Tmux(TmuxCommands),
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
pub enum TmuxCommands {
    /// Publish every project's attention snapshot as `@pm_*` tmux options,
    /// alerting attached clients when a feature becomes blocked or ready
    Refresh,
    /// Install pm's tmux plugin on the running server; put
    /// `run-shell 'pm tmux init'` in your tmux config
    Init,
    /// Switch a client to a tree item, or to the agent a feature is waiting
    /// on (the Enter action of pm's tree)
    Jump {
        /// The client to switch (`#{client_name}`)
        #[arg(long)]
        client: String,
        /// The chosen item's target, as `choose-tree` gives `%%`
        target: String,
    },
    /// Refresh every `@pm-refresh-interval` seconds while the server runs
    /// (started by `pm tmux init`; one per server)
    #[command(hide = true)]
    Watch,
    /// Refresh while a watcher runs on the server (started by pm commands
    /// that change what the options show)
    #[command(hide = true)]
    Push,
}

#[derive(Subcommand)]
pub enum WorkflowCommands {
    /// Print the active workflow's routing prose (workflow.md)
    Show,
    /// List installed workflows (project tier first, then global) with
    /// their descriptions
    List,
}

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
        /// Harness whose sessions to import
        #[arg(long, default_value = "claude-code")]
        harness: Harness,
    },
    /// List the harnesses pm can spawn
    List,
    /// Check the installed harness binary for the capabilities pm relies on
    Probe {
        /// Harness to probe
        #[arg(long, default_value = "claude-code")]
        harness: Harness,
    },
}

#[derive(Subcommand)]
pub enum HarnessAgentsCommands {
    /// List bundled agent definitions, their global install status, and any project override
    List,
    /// Uninstall bundled agent definitions from ~/.agents/agents/ (and its projections)
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
    /// List settings files (main or feature)
    List {
        /// Feature name (detected from CWD if omitted; works from main worktree too)
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
    Stop,
    /// SessionStart hook handler — called by the harness on session start (not for direct use)
    SessionStart,
    /// UserPromptSubmit hook handler — unblocks a feature on the user's input (not for direct use)
    UserPrompt,
    /// Status hook handler — records when an agent waits on the user (not for direct use)
    Waiting {
        /// The harness whose hook payload arrives on stdin
        harness: pm::harness::Harness,
    },
}

#[derive(Subcommand)]
pub enum AgentCommands {
    /// Spawn an agent in a tmux window
    Spawn {
        /// Agent name (omit to respawn all previously active agents).
        /// Used as the registry key, tmux window name, and `PM_AGENT_NAME`.
        name: Option<String>,
        /// Agent definition to launch (`main/.agents/agents/<name>.md` or
        /// `~/.agents/agents/`). Defaults to `name` when omitted. Use this to
        /// spawn multiple agents from the same definition under different
        /// display names, e.g. `pm agent spawn frontend-dev --agent implementer`.
        #[arg(long = "agent", value_name = "DEFINITION")]
        agent_definition: Option<String>,
        /// Initial context for the agent. Use `-` to read the body from stdin
        /// (e.g. `--context - <<'EOF' … EOF`).
        #[arg(long)]
        context: Option<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
    },
    /// List agents in the current feature
    List {
        /// Only show active agents
        #[arg(long)]
        active: bool,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Stop one or more running agents (kill window, mark inactive)
    Stop {
        /// Agent name(s)
        #[arg(required = true)]
        names: Vec<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Delete one or more agents (kill window and remove registry entry entirely; cannot be respawned)
    Delete {
        /// Agent name(s)
        #[arg(required = true)]
        names: Vec<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Restart one or more agents (stop then respawn, preserving active flag and session)
    Restart {
        /// Agent name(s)
        #[arg(required = true)]
        names: Vec<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Fork an existing agent: spawn a new agent that starts with a copy of the source's conversation history
    Fork {
        /// Source agent name to fork from (can be running or stopped)
        source: String,
        /// New agent name
        name: String,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
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
        /// Deliver to a different project (by registered name)
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
    /// Reply to the last-read message (auto-routes cross-scope)
    Reply {
        /// Reply body (if omitted, reads from stdin)
        message: Option<String>,
        /// Sender identity (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
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
        /// Initial context (literal text, path to a file, or `-` to read the
        /// body from stdin, e.g. `--context - <<'EOF' … EOF`)
        #[arg(long)]
        context: Option<String>,
        /// Base branch to stack on (defaults to current branch from CWD)
        #[arg(long)]
        base: Option<String>,
        /// Workflow name (defaults to 'solo' when --context is given).
        /// Run `pm workflow list` for installed workflows.
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Adopt an existing branch as a feature (worktree + tmux session)
    Adopt {
        /// Branch name to adopt (slashes are sanitized to dashes for the feature name)
        name: String,
        /// Override the derived feature name
        #[arg(long)]
        feature_name: Option<String>,
        /// Initial context (literal text, path to a file, or `-` to read the
        /// body from stdin, e.g. `--context - <<'EOF' … EOF`)
        #[arg(long)]
        context: Option<String>,
        /// Migrate harness sessions from this old path
        #[arg(long)]
        from: Option<PathBuf>,
        /// Workflow name (defaults to 'solo' when --context is given).
        /// Run `pm workflow list` for installed workflows.
        #[arg(long)]
        workflow: Option<String>,
    },
    /// List all features with their status, lifecycle, branch, base and PR
    List {
        /// List the features of every registered project
        #[arg(long)]
        all: bool,
    },
    /// Show detailed info for a feature
    Info {
        /// Feature name (detected from CWD if omitted)
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
    /// GitHub PR management
    #[command(subcommand)]
    Pr(PrCommands),
    /// Set where the feature's work stands: wip, blocked (waiting on the
    /// user), or ready (done; notifies the orchestrator, needs a summary).
    /// Without a status, show what needs attention: this feature, or every
    /// feature, most urgent first, when run from main or with --all
    Status {
        /// wip, blocked or ready; omit to view. Alone, a value that is not
        /// a status is taken as the feature name to view
        status: Option<String>,
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// What the user is to answer (only with `blocked`)
        #[arg(short = 'm', long)]
        reason: Option<String>,
        /// View every feature of every registered project
        #[arg(long, conflicts_with_all = ["status", "name", "reason"])]
        all: bool,
        /// View as JSON (the attention snapshot; see README)
        #[arg(long)]
        json: bool,
    },
    /// The feature's summary for the orchestrator (kept in pm state, not the branch)
    #[command(subcommand)]
    Summary(SummaryCommands),
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
pub enum SummaryCommands {
    /// Print the summary file's path, to edit like any other file
    Path {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Print the feature's summary
    Show {
        /// Feature name (detected from CWD if omitted)
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
        /// PR body (literal text, path to a file, or `-` to read from stdin;
        /// overrides template)
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
        /// New PR body (literal text, path to a file, or `-` to read from stdin)
        #[arg(long)]
        body: Option<String>,
    },
    /// Mark a feature's draft PR as ready for review
    Ready {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
}
