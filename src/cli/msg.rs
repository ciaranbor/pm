use clap::Subcommand;

use super::ProjectFlag;

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
