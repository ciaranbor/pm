use clap::{Parser, Subcommand};
use std::path::PathBuf;

use pm::commands;
use pm::error::PmError;
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
    /// Claude Code permissions management
    #[command(subcommand)]
    Perm(PermCommands),
}

#[derive(Subcommand)]
enum PermCommands {
    /// List a feature's Claude Code permissions
    List {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Push feature's .claude/ settings to main
    Push {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Pull main's .claude/ settings into a feature
    Pull {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Show differences between template and feature permissions
    Diff {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Merge feature and main permissions (union), writing result to main
    Merge {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// On scalar conflicts, let the feature (ours) win instead of main (theirs)
        #[arg(long)]
        ours: bool,
    },
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
    /// Adopt an existing branch as a feature (worktree + tmux session)
    Adopt {
        /// Branch name to adopt
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
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Skip safety checks
        #[arg(long)]
        force: bool,
    },
    /// Merge a feature branch into the base branch
    Merge {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Clean up after merge (kill session, remove worktree, delete branch, remove state)
        #[arg(long)]
        delete: bool,
    },
    /// Create or link a GitHub PR for a feature
    Pr {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Create a non-draft (ready) PR instead of draft
        #[arg(long)]
        ready: bool,
    },
    /// Mark a feature's PR as ready for review
    Ready {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
}

fn resolve_feature_name(
    name: Option<String>,
    project_root: &std::path::Path,
) -> pm::error::Result<String> {
    name.or_else(|| paths::detect_feature_from_cwd(project_root, &std::env::current_dir().ok()?))
        .ok_or(PmError::NotInFeatureWorktree)
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
        Commands::Perm(perm_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match perm_cmd {
                PermCommands::List { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    let lines = commands::permissions::list(&project_root, &name)?;
                    if lines.is_empty() {
                        println!("No permissions files found for feature '{name}'");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Ok(())
                }
                PermCommands::Push { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::permissions::push(&project_root, &name)?;
                    println!("Pushed permissions from feature '{name}' to main");
                    Ok(())
                }
                PermCommands::Pull { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::permissions::pull(&project_root, &name)?;
                    println!("Pulled permissions from main into feature '{name}'");
                    Ok(())
                }
                PermCommands::Diff { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    let lines = commands::permissions::diff(&project_root, &name)?;
                    if lines.is_empty() {
                        println!("No differences");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Ok(())
                }
                PermCommands::Merge { name, ours } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::permissions::merge(&project_root, &name, ours)?;
                    println!("Merged permissions from feature '{name}' into main");
                    Ok(())
                }
            }
        }
        Commands::Feat(feat_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match feat_cmd {
                FeatCommands::New { name, context } => {
                    commands::feat_new::feat_new(&project_root, &name, context.as_deref(), None)?;
                    println!("Created feature '{name}'");
                    Ok(())
                }
                FeatCommands::Adopt { name, context } => {
                    commands::feat_adopt::feat_adopt(
                        &project_root,
                        &name,
                        context.as_deref(),
                        None,
                    )?;
                    println!("Adopted feature '{name}'");
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
                    let name = name.or_else(|| {
                        paths::detect_feature_from_cwd(
                            &project_root,
                            &std::env::current_dir().ok()?,
                        )
                    });
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
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::feat_delete::feat_delete(&project_root, &name, force, None)?;
                    println!("Deleted feature '{name}'");
                    Ok(())
                }
                FeatCommands::Merge { name, delete } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::feat_merge::feat_merge(&project_root, &name, delete, None)?;
                    if delete {
                        println!("Merged and deleted feature '{name}'");
                    } else {
                        println!("Merged feature '{name}'");
                    }
                    Ok(())
                }
                FeatCommands::Pr { name, ready } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::feat_pr::feat_pr(&project_root, &name, ready)?;
                    println!("PR linked for feature '{name}'");
                    Ok(())
                }
                FeatCommands::Ready { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::feat_ready::feat_ready(&project_root, &name)?;
                    println!("PR marked ready for feature '{name}'");
                    Ok(())
                }
            }
        }
    }
}
