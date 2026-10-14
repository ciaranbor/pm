use clap::{ArgGroup, Args, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

use pm::harness::Harness;

#[derive(Parser)]
#[command(
    name = "pm",
    version = pm::version::VERSION,
    about = "Terminal-based project manager built around tmux and git worktrees"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a pm project from a new repo, or by cloning a remote one (--git)
    Init {
        /// Path for the new project root; its last component names the
        /// project. Defaults to `./<repo name>` when cloning with --git
        #[arg(required_unless_present = "git")]
        path: Option<PathBuf>,
        /// Clone a remote repo instead of running git init
        #[arg(long)]
        git: Option<String>,
        /// Don't spawn the `main` agent in the main session
        #[arg(long)]
        no_main: bool,
    },
    /// Make an existing local repo a pm project, symlinked into a `<name>-pm/` wrapper
    /// (or moved there with --move)
    Register {
        /// Path to the existing git repo
        path: PathBuf,
        /// Custom project name (defaults to directory name)
        #[arg(long)]
        name: Option<String>,
        /// Move the repo into the wrapper instead of symlinking
        #[arg(long, rename_all = "kebab-case")]
        r#move: bool,
        /// Don't spawn the `main` agent in the main session
        #[arg(long)]
        no_main: bool,
    },
    /// Set up this machine's home project and start its supervisor, an
    /// agent that changes pm settings and acts on any project for you
    ///
    /// Reports which harnesses are ready, asks which one the supervisor
    /// runs on, then creates home and attaches to it. Once home exists, it
    /// just opens home.
    Onboard {
        /// The supervisor's harness, instead of asking
        #[arg(long)]
        harness: Option<Harness>,
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
    /// The agent CLIs pm launches: hooks, bundled skills and agents, sessions
    #[command(subcommand)]
    Harness(HarnessCommands),
    /// Close all tmux sessions for a project (counterpart to `pm open`)
    Close {
        /// Project name (defaults to current project from CWD)
        #[arg(long, conflicts_with = "all")]
        project: Option<String>,
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
    /// Edit the project's notes (`.pm/notes.md`) in $VISUAL or $EDITOR
    ///
    /// One Markdown file per project, outside every worktree, synced by
    /// `pm state push` and editable from the phone app through `pm serve`.
    Notes {
        /// Project name (defaults to current project from CWD)
        project: Option<String>,
    },
    /// Read the project's information store: list its categories, or show
    /// one
    Docs {
        /// The category to show, by filename, with or without `.md`
        /// (e.g. `todo`); lists the categories when omitted
        category: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
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
    /// Install the latest pm release if it is newer than this pm, then reinstall
    /// pm's hooks and bundled assets in every registered project and restart
    /// the agents left stale
    ///
    /// A release check that fails (offline, GitHub unreachable) is reported and
    /// the projects are upgraded with this pm.
    #[command(visible_alias = "upgrade")]
    Update {
        /// Skip the release check: only upgrade the projects, with this pm
        #[arg(long)]
        local: bool,
        /// Preview changes without writing anything. Stale agents are
        /// judged against the assets installed now, not the ones the
        /// update would install
        #[arg(long, alias = "check")]
        dry_run: bool,
        /// Ignored: every project is always upgraded; pm 0.5.0 and earlier
        /// pass it after installing a release
        #[arg(long, hide = true)]
        all: bool,
    },
    /// Set up registered projects on a machine that lacks them, from the global registry
    Restore {
        /// A registered project to restore (repeatable; default: every one)
        #[arg(long = "project", value_name = "NAME")]
        projects: Vec<String>,
        /// A `pm harness export` tarball whose sessions to import before
        /// agents are started (repeatable: one per harness)
        #[arg(long = "import", value_name = "TARBALL")]
        imports: Vec<PathBuf>,
    },
    /// Check that projects can move to another machine (`pm restore` sets them up there)
    #[command(subcommand)]
    Migrate(MigrateCommands),
    /// Read and change pm settings: the global config.toml and each
    /// project's .pm/config.toml
    ///
    /// Keys are dotted paths into the config files, e.g.
    /// `agents.models.reviewer`, `harness.codex.sandbox` or `serve.port`; an
    /// unknown key is refused with the valid ones listed.
    #[command(subcommand)]
    Config(ConfigCommands),
    /// Git-backed state management (.pm/ backup and sync)
    #[command(subcommand)]
    State(StateCommands),
    /// Workflows: each scope's team and who hands off to whom
    #[command(subcommand)]
    Workflow(WorkflowCommands),
    /// tmux integration: pm state published on the tmux server
    #[command(subcommand)]
    Tmux(TmuxCommands),
    /// Serve pm state, and input to agents, over HTTP for the phone app, on 127.0.0.1;
    /// `pm serve install` sets it up to run at login, reached through
    /// `tailscale serve`
    #[command(args_conflicts_with_subcommands = true)]
    Serve {
        #[command(subcommand)]
        command: Option<ServeCommands>,
        /// Loopback port to listen on (default: `[serve] port`, else 7764)
        #[arg(long)]
        port: Option<u16>,
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
pub enum ConfigCommands {
    /// Show a setting's effective value and where it came from (project,
    /// then global, else unset), resolved as a spawn resolves it
    Get {
        /// The setting, e.g. agents.models.reviewer
        key: String,
        #[command(flatten)]
        tier: ConfigTier,
    },
    /// Set a setting in the project's config, or the global one with --global
    Set {
        /// The setting, e.g. agents.models.reviewer
        key: String,
        /// The value: text as given, `true`/`false`, a number, a list as
        /// `a,b` or a TOML array, a provider entry as a TOML inline table.
        /// An empty value masks the global one where the key allows it
        #[arg(allow_hyphen_values = true)]
        value: String,
        #[command(flatten)]
        tier: ConfigTier,
    },
    /// Remove a setting from the project's config, or the global one with
    /// --global
    Unset {
        /// The setting, e.g. agents.models.reviewer
        key: String,
        #[command(flatten)]
        tier: ConfigTier,
    },
    /// List every setting either file holds, with its effective value
    List {
        /// Only the settings under this key, e.g. agents.models
        prefix: Option<String>,
        #[command(flatten)]
        tier: ConfigTier,
    },
}

/// Which config file a `pm config` command acts on.
#[derive(Args)]
pub struct ConfigTier {
    /// Project name (defaults to current project from CWD)
    #[arg(long, conflicts_with = "global")]
    pub project: Option<String>,
    /// The global config (~/.config/pm/config.toml) alone
    #[arg(long)]
    pub global: bool,
}

#[derive(Subcommand)]
pub enum MigrateCommands {
    /// Report what moving projects to another machine would lose or fail
    /// on, with the command that fixes each; read-only, and exits non-zero
    /// while anything blocks
    Check {
        /// A registered project to check (repeatable; default: every one)
        #[arg(long = "project", value_name = "NAME")]
        projects: Vec<String>,
        /// Also list the files behind each count and why each item matters
        #[arg(long, short)]
        verbose: bool,
        /// Print the report as JSON
        #[arg(long, conflicts_with = "verbose")]
        json: bool,
    },
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
pub enum ServeCommands {
    /// Pair a device: print a QR code of the server's URL and a new token
    Pair {
        /// The device's name, used to revoke it and in the request log
        #[arg(long, default_value = "phone")]
        name: String,
        /// The URL the device reaches the server at (default: this machine's
        /// tailnet name over HTTPS, as `tailscale serve` serves it)
        #[arg(long)]
        url: Option<String>,
    },
    /// List paired devices
    Devices,
    /// Revoke a device's token
    Revoke {
        /// The device's name
        device: String,
    },
    /// Run `pm serve` as a launchd LaunchAgent (macOS), at login and across
    /// restarts, and set up `tailscale serve` for it when the tailnet
    /// allows. Run it again to repair an install
    Install {
        /// Loopback port to listen on, saved as `[serve] port`
        #[arg(long)]
        port: Option<u16>,
        /// Leave `tailscale serve` as it is
        #[arg(long)]
        no_tailscale: bool,
        /// Also pair a device with this name, printing its QR code
        #[arg(long, value_name = "DEVICE")]
        pair: Option<String>,
    },
    /// Remove the LaunchAgent `install` wrote
    Uninstall,
    /// Whether the server runs, and what reaches it
    Status,
    /// Print the end of the server's log
    Logs {
        /// How many lines
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep printing what is appended
        #[arg(short, long)]
        follow: bool,
    },
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
        /// Run hooks codex has not been told to trust
        #[arg(long)]
        bypass_hook_trust: bool,
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
        /// `plain` launches a vanilla session with no definition.
        #[arg(long = "agent", value_name = "DEFINITION")]
        agent_definition: Option<String>,
        /// Initial context for the agent. Use `-` to read the body from stdin
        /// (e.g. `--context - <<'EOF' … EOF`).
        #[arg(long)]
        context: Option<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// List agents in the current feature
    List {
        /// Only show active agents
        #[arg(long)]
        active: bool,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Stop one or more running agents (kill window, mark inactive)
    Stop {
        /// Agent name(s)
        #[arg(required = true)]
        names: Vec<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Delete one or more agents (kill window and remove registry entry entirely; cannot be respawned)
    Delete {
        /// Agent name(s)
        #[arg(required = true)]
        names: Vec<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Restart one or more agents (stop then respawn, preserving active flag and session)
    Restart {
        /// Agent name(s)
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        names: Vec<String>,
        /// Restart every active agent of the scope; agents that are
        /// mid-turn, asking, or running background work, and those of a
        /// closed feature, are skipped and reported
        #[arg(long)]
        all: bool,
        /// With --all: every scope of every registered project
        #[arg(long, requires = "all", conflicts_with_all = ["scope", "project"])]
        global: bool,
        /// With --all: only running agents that would launch differently
        /// now; the agent running the command, and any mid-turn, restart at
        /// their next idle instead
        #[arg(long, requires = "all")]
        stale: bool,
        /// Restart an agent that is mid-turn, asking, or running background
        /// work; it is sent a message telling it to resume. With --all, every
        /// such agent is interrupted
        #[arg(long)]
        force: bool,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Show the end of an agent's conversation, readably, whatever its
    /// harness
    Log {
        /// Agent name
        name: String,
        /// How many of the conversation's last entries to show
        #[arg(short = 'n', long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=200))]
        lines: u16,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Show an agent's permission mode, or switch it (Claude Code and codex
    /// agents)
    Mode {
        /// Agent name
        name: String,
        /// The mode to switch to, in the harness's own ids (run without it to
        /// list them)
        mode: Option<String>,
        /// Target scope (feature name or "main"; defaults to current scope)
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
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
        #[command(flatten)]
        target: ProjectFlag,
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
        /// Deliver to a different scope (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
        /// Deliver to the parent scope (base branch's feature). Shorthand for --scope <base>.
        #[arg(long, conflicts_with = "scope")]
        upstream: bool,
        /// Deliver to a different project (by registered name): its main scope unless --scope names another
        #[arg(long, conflicts_with = "upstream")]
        project: Option<String>,
        /// Hold the message until this long from now (e.g. 30s, 10m, 2h, 1d, 1h30m). The
        /// recipient's project's waiters deliver it, on this machine only
        #[arg(long = "in", value_name = "DURATION", value_parser = pm::commands::msg_scheduled::parse_delay)]
        delay: Option<chrono::Duration>,
    },
    /// List the project's scheduled sends (`pm msg send --in`), soonest due first
    Scheduled {
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Cancel a scheduled send
    Cancel {
        /// The send's id, or a leading part of it naming one send
        id: String,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Read the next unread message and advance the cursor
    Read {
        /// Which sender's queue to read from: its address (name@scope, plus
        /// @project for another project's), or any leading part of it that
        /// names one sender. Required with --index. Without it, a read takes
        /// the sender whose earliest unread message is oldest.
        #[arg(long)]
        from: Option<String>,
        /// Re-read a specific message without advancing the cursor.
        /// Absolute index ("3"), or relative to the cursor ("+2", "-1").
        /// If omitted, reads the next unread message and advances.
        #[arg(long, value_name = "SPEC")]
        index: Option<String>,
        /// Read from a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// List all messages in your inbox, with cursor position markers
    List {
        /// Only show messages from the senders this address, or leading part of one, names
        #[arg(long)]
        from: Option<String>,
        /// List messages from a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Reply to the last-read message (auto-routes cross-scope)
    Reply {
        /// Reply body (if omitted, reads from stdin)
        message: Option<String>,
        /// Reply from a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Block until a message arrives in your inbox
    Wait {
        /// Only block on messages from the senders this address, or leading part of one, names
        #[arg(long)]
        from: Option<String>,
        /// Wait on a different scope's inbox (feature name or "main")
        #[arg(long)]
        scope: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
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
        /// Base on the main branch, starting from its copy on the remote,
        /// fetched now (the default when the `fresh_base` config is set and
        /// the base is the main branch)
        #[arg(long, conflicts_with_all = ["base", "no_fresh"])]
        fresh: bool,
        /// Start from the local base even when `fresh_base` is set
        #[arg(long)]
        no_fresh: bool,
        /// Workflow name (defaults to 'solo' when --context is given).
        /// Run `pm workflow list` for installed workflows.
        #[arg(long)]
        workflow: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
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
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// List all features with their status, lifecycle, branch, base and PR
    List {
        /// List the features of every registered project
        #[arg(long, conflicts_with = "project")]
        all: bool,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Show detailed info for a feature
    Info {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Delete a feature (with safety checks)
    Delete {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Skip safety checks
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Merge a feature branch into the base branch
    Merge {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Keep the feature after merge (preserve session, worktree, branch, and state)
        #[arg(long)]
        keep: bool,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// GitHub PR management
    #[command(subcommand)]
    Pr(PrCommands),
    /// Set where the scope's work stands: wip, blocked (waiting on the
    /// user), or, for a feature only, ready (done; notifies the
    /// orchestrator, needs a summary). Works in main too, which is never
    /// ready. Without a status, show what needs attention: this feature, or
    /// main and every feature, most urgent first, when run from main or
    /// with --all
    Status {
        /// wip, blocked or ready; omit to view. Alone, a value that is not
        /// a status is taken as the scope name to view (`main` included)
        status: Option<String>,
        /// Feature name, or `main` (detected from CWD if omitted)
        name: Option<String>,
        /// What the user is to answer (only with `blocked`)
        #[arg(short = 'm', long)]
        reason: Option<String>,
        /// View every feature of every registered project
        #[arg(long, conflicts_with_all = ["status", "name", "reason", "project"])]
        all: bool,
        /// View as JSON (the attention snapshot; see docs/remote-api.md)
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// The feature's summary for the orchestrator (kept in pm state, not the branch)
    #[command(subcommand)]
    Summary(SummaryCommands),
    /// Check out a PR for review (fetch branch + worktree + tmux session)
    Review {
        /// PR number or GitHub PR URL
        pr: String,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Sync feature statuses with their linked GitHub PRs
    Sync {
        /// Feature name (syncs just this feature). Detected from CWD if omitted. If not in a feature worktree, syncs all features.
        name: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
}

#[derive(Subcommand)]
pub enum SummaryCommands {
    /// Print the summary file's path, to edit like any other file
    Path {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Print the feature's summary
    Show {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
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
        #[command(flatten)]
        target: ProjectFlag,
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
        #[command(flatten)]
        target: ProjectFlag,
    },
    /// Mark a feature's draft PR as ready for review
    Ready {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        #[command(flatten)]
        target: ProjectFlag,
    },
}

/// `--project`: the registered project a command acts on, wherever it runs.
#[derive(Args)]
pub struct ProjectFlag {
    /// Project name (defaults to current project from CWD)
    #[arg(long)]
    pub project: Option<String>,
}
