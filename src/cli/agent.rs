use clap::Subcommand;

use super::ProjectFlag;

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
