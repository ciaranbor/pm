use clap::Subcommand;

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
