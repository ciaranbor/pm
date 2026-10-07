use pm::commands;
use pm::error::{PmError, Result};
use pm::state::paths;

use crate::cli::StateCommands;

pub(super) fn run(cmd: StateCommands) -> Result<()> {
    match cmd {
        StateCommands::Init { global, remote } => {
            let msg = if global {
                commands::state_cmd::global_init_with_remote(remote.as_deref())?
            } else {
                let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                commands::state_cmd::init_with_remote(&project_root, remote.as_deref())?
            };
            println!("{msg}");
            Ok(())
        }
        StateCommands::Remote { url, global } => {
            let msg = if global {
                let u = url.ok_or_else(|| {
                    PmError::Git(
                        "--global requires a URL (interactive mode not supported for global registry)".to_string(),
                    )
                })?;
                commands::state_cmd::global_remote(&u)?
            } else {
                let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                commands::state_cmd::remote(&project_root, url.as_deref())?
            };
            println!("{msg}");
            Ok(())
        }
        StateCommands::Push { global } => {
            let msg = if global {
                commands::state_cmd::global_push()?
            } else {
                let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                commands::state_cmd::push(&project_root)?
            };
            println!("{msg}");
            Ok(())
        }
        StateCommands::Pull { global } => {
            let msg = if global {
                commands::state_cmd::global_pull()?
            } else {
                let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                commands::state_cmd::pull(&project_root)?
            };
            println!("{msg}");
            Ok(())
        }
        StateCommands::Status { global } => {
            let msg = if global {
                commands::state_cmd::global_status()?
            } else {
                let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                commands::state_cmd::status(&project_root)?
            };
            println!("{msg}");
            Ok(())
        }
        StateCommands::Backfill => {
            let messages = commands::state_cmd::backfill()?;
            for msg in messages {
                println!("{msg}");
            }
            Ok(())
        }
    }
}
