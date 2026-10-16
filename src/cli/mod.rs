use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

use pm::harness::Harness;

mod agent;
mod config;
mod feat;
mod harness;
mod msg;
mod serve;
mod state;
mod workflow;

pub use agent::AgentCommands;
pub use config::{ConfigCommands, ConfigTier};
pub use feat::{FeatCommands, PrCommands, SummaryCommands};
pub use harness::{
    HarnessAgentsCommands, HarnessCommands, HarnessHooksCommands, HarnessSettingsCommands,
    HarnessSkillsCommands,
};
pub use msg::MsgCommands;
pub use serve::ServeCommands;
pub use state::StateCommands;
pub use workflow::WorkflowCommands;

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

/// `--project`: the registered project a command acts on, wherever it runs.
#[derive(Args)]
pub struct ProjectFlag {
    /// Project name (defaults to current project from CWD)
    #[arg(long)]
    pub project: Option<String>,
}
