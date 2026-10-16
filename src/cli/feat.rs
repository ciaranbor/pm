use clap::{ArgGroup, Subcommand};
use std::path::PathBuf;

use super::ProjectFlag;

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
