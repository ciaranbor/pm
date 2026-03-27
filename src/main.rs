use clap::{Parser, Subcommand};
use std::path::PathBuf;

use pm::commands;
use pm::state::paths;
use pm::tmux;

#[derive(Parser)]
#[command(
    name = "pm",
    about = "Terminal-based project manager built around tmux and git worktrees"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new pm project with a git repo
    Init {
        /// Path for the new project root
        path: PathBuf,
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
    /// Open/reconstruct tmux sessions for the current project
    Open,
    /// Feature management
    #[command(subcommand)]
    Feat(FeatCommands),
}

#[derive(Subcommand)]
enum FeatCommands {
    /// Create a new feature (branch + worktree + tmux session)
    New {
        /// Feature name
        name: String,
        /// Initial context (literal text or path to a file)
        #[arg(long)]
        context: Option<String>,
    },
    /// List all features with their status
    List,
    /// Switch to a feature's tmux session
    Switch {
        /// Feature name (omit for interactive picker)
        name: Option<String>,
    },
    /// Delete a feature (with safety checks)
    Delete {
        /// Feature name
        name: String,
        /// Skip safety checks
        #[arg(long)]
        force: bool,
    },
}

fn main() {
    let result = run();
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> pm::error::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => {
            let projects_dir = paths::global_projects_dir()?;
            commands::init::init(&path, &projects_dir, None)
        }
        Commands::Register { path, name, r#move } => {
            let projects_dir = paths::global_projects_dir()?;
            commands::register::register(&path, name.as_deref(), &projects_dir, r#move, None)
        }
        Commands::List => {
            let projects_dir = paths::global_projects_dir()?;
            let lines = commands::list::list_projects(&projects_dir)?;
            if lines.is_empty() {
                println!("No projects");
            } else {
                for line in lines {
                    println!("{line}");
                }
            }
            Ok(())
        }
        Commands::Open => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            commands::open::open(&project_root, None)?;
            println!("Project sessions opened");
            Ok(())
        }
        Commands::Feat(feat_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match feat_cmd {
                FeatCommands::New { name, context } => {
                    commands::feat_new::feat_new(&project_root, &name, context.as_deref(), None)?;
                    println!("Created feature '{name}'");
                    Ok(())
                }
                FeatCommands::List => {
                    let lines = commands::feat_list::feat_list(&project_root)?;
                    if lines.is_empty() {
                        println!("No features");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Ok(())
                }
                FeatCommands::Switch { name } => {
                    if let Some(name) = name {
                        commands::feat_switch::feat_switch(&project_root, &name, None)
                    } else {
                        let items = commands::feat_switch::feat_switch_menu(&project_root)?;
                        let pm_dir = paths::pm_dir(&project_root);
                        let config = pm::state::project::ProjectConfig::load(&pm_dir)?;
                        tmux::display_menu(
                            None,
                            &format!("{} features", config.project.name),
                            &items,
                        )
                    }
                }
                FeatCommands::Delete { name, force } => {
                    commands::feat_delete::feat_delete(&project_root, &name, force, None)?;
                    println!("Deleted feature '{name}'");
                    Ok(())
                }
            }
        }
    }
}
