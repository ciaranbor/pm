use pm::commands::config::{self, Files, Tier};
use pm::error::{PmError, Result};
use pm::state::paths;

use super::scope::{optional_project_root, project_root};
use crate::cli::{ConfigCommands, ConfigTier};

pub(super) fn run(cmd: ConfigCommands) -> Result<()> {
    let lines = match cmd {
        ConfigCommands::Get { key, tier } => {
            vec![config::get(&files(&tier)?, &key, tier.global)?.to_string()]
        }
        ConfigCommands::List { prefix, tier } => {
            config::list(&files(&tier)?, tier.global, prefix.as_deref())?
        }
        ConfigCommands::Set { key, value, tier } => {
            let files = files(&tier)?;
            config::set(&files, write_tier(&files, &tier)?, &key, &value)?
        }
        ConfigCommands::Unset { key, tier } => {
            let files = files(&tier)?;
            config::unset(&files, write_tier(&files, &tier)?, &key)?
        }
    };
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn files(tier: &ConfigTier) -> Result<Files> {
    let project_root = match &tier.project {
        Some(name) => Some(project_root(&paths::global_projects_dir()?, Some(name))?),
        None => optional_project_root()?,
    };
    Ok(Files {
        project_root,
        global_dir: paths::global_config_dir()?,
    })
}

fn write_tier(files: &Files, tier: &ConfigTier) -> Result<Tier> {
    match (tier.global, &files.project_root) {
        (true, _) => Ok(Tier::Global),
        (false, Some(_)) => Ok(Tier::Project),
        (false, None) => Err(PmError::Config(
            "not in a pm project: pass --project <name>, or --global for the global config"
                .to_string(),
        )),
    }
}
