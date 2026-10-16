use clap::{Args, Subcommand};

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
