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
    /// Agent management (spawn, list)
    #[command(subcommand)]
    Agent(AgentCommands),
    /// Inter-agent messaging (send, read, next, list, wait)
    #[command(subcommand)]
    Msg(MsgCommands),
    /// Claude Code settings, skills, and session management
    #[command(subcommand)]
    Claude(ClaudeCommands),
    /// Delete a project (teardown features, sessions, state, and registry entry)
    Delete {
        /// Project name (defaults to current project from CWD)
        #[arg(long)]
        project: Option<String>,
        /// Skip safety checks and force-remove worktree directories
        #[arg(long)]
        force: bool,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Show project dashboard (features, PR status, health)
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
}

#[derive(Subcommand)]
enum ClaudeCommands {
    /// Per-feature Claude Code settings
    #[command(subcommand)]
    Settings(ClaudeSettingsCommands),
    /// Manage bundled Claude Code skills
    #[command(subcommand)]
    Skills(ClaudeSkillsCommands),
    /// Manage bundled Claude Code agent definitions
    #[command(subcommand)]
    Agents(ClaudeAgentsCommands),
    /// Claude Code lifecycle hooks managed by pm
    #[command(subcommand)]
    Hooks(HooksCommands),
    /// Migrate Claude Code sessions from an old project path to the current directory
    Migrate {
        /// The old absolute path where the project previously lived
        #[arg(long)]
        from: PathBuf,
    },
}

#[derive(Subcommand)]
enum ClaudeAgentsCommands {
    /// List available bundled agent definitions and their install status
    List,
    /// Uninstall bundled agent definitions (project-level by default, or --global)
    Uninstall {
        /// Agent name (required unless --all is passed)
        name: Option<String>,
        /// Uninstall all bundled agent definitions
        #[arg(long)]
        all: bool,
        /// Uninstall from ~/.claude/agents/ instead of the project
        #[arg(long)]
        global: bool,
    },
    /// Install bundled agent definitions (project-level by default, or --global for ~/.claude/agents/)
    Install {
        /// Agent name (installs all if omitted)
        name: Option<String>,
        /// Install to ~/.claude/agents/ instead of the project
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum ClaudeSettingsCommands {
    /// List Claude Code settings (main or feature)
    List {
        /// Feature name (detected from CWD if omitted; works from main worktree too)
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
    /// Show differences between main and feature settings
    Diff {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Merge feature and main settings (union), writing result to main
    Merge {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// On scalar conflicts, let the feature (ours) win instead of main (theirs)
        #[arg(long)]
        ours: bool,
    },
}

#[derive(Subcommand)]
enum ClaudeSkillsCommands {
    /// List available bundled skills and their install status
    List,
    /// Install bundled skills (project-level by default, or --global for ~/.claude/skills/)
    Install {
        /// Skill name (installs all if omitted)
        name: Option<String>,
        /// Install to ~/.claude/skills/ instead of the project
        #[arg(long)]
        global: bool,
    },
    /// Uninstall bundled skills (project-level by default, or --global)
    Uninstall {
        /// Skill name (required unless --all is passed)
        name: Option<String>,
        /// Uninstall all bundled skills
        #[arg(long)]
        all: bool,
        /// Uninstall from ~/.claude/skills/ instead of the project
        #[arg(long)]
        global: bool,
    },
    /// Pull skills from main into a feature's .claude/skills/
    Pull {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Install the pm Stop hook into main/.claude/settings.json
    Install,
    /// Stop hook handler — called by Claude Code on every Stop event (not for direct use)
    Stop,
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Spawn an agent in a tmux window
    Spawn {
        /// Agent name (omit to respawn all previously active agents)
        name: Option<String>,
        /// Initial context for the agent
        #[arg(long)]
        context: Option<String>,
        /// Enable acceptEdits permission mode
        #[arg(long)]
        edit: bool,
    },
    /// List agents in the current feature
    List {
        /// Only show active agents
        #[arg(long)]
        active: bool,
    },
}

#[derive(Subcommand)]
enum MsgCommands {
    /// Send a message to an agent's inbox
    Send {
        /// Recipient agent name
        agent: String,
        /// Message body
        message: String,
        /// Sender identity (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
    },
    /// Read a single message from your inbox (does not advance the cursor)
    Read {
        /// Which sender's queue to read from. Required with --index.
        /// Without --index, inferred when exactly one sender has unread messages.
        #[arg(long)]
        from: Option<String>,
        /// Absolute index ("3"), or relative to the cursor ("+2", "-1").
        /// If omitted, reads the next unread message (cursor + 1).
        #[arg(long, value_name = "SPEC")]
        index: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
    },
    /// Advance a sender's cursor by one message
    Next {
        /// Which sender's cursor to advance. Inferred when exactly one sender
        /// has unread messages.
        #[arg(long)]
        from: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
    },
    /// List all messages in your inbox, with cursor position markers
    List {
        /// Only show messages from this sender
        #[arg(long)]
        from: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
    },
    /// Block until a message arrives in your inbox
    Wait {
        /// Only block on messages from this sender
        #[arg(long)]
        from: Option<String>,
        /// Agent name (defaults to $PM_AGENT_NAME or $USER)
        #[arg(long)]
        as_agent: Option<String>,
    },
}

#[derive(Subcommand)]
enum FeatCommands {
    /// Create a new feature (branch + worktree + tmux session)
    New {
        /// Branch name (slashes are sanitized to dashes for the feature name)
        name: String,
        /// Override the derived feature name
        #[arg(long)]
        feature_name: Option<String>,
        /// Initial context (literal text or path to a file)
        #[arg(long)]
        context: Option<String>,
        /// Base branch to stack on (defaults to current branch from CWD)
        #[arg(long)]
        base: Option<String>,
        /// Don't auto-accept edits in the spawned Claude session
        #[arg(long)]
        no_edit: bool,
        /// Agent to spawn (overrides project default)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Adopt an existing branch as a feature (worktree + tmux session)
    Adopt {
        /// Branch name to adopt (slashes are sanitized to dashes for the feature name)
        name: String,
        /// Override the derived feature name
        #[arg(long)]
        feature_name: Option<String>,
        /// Initial context (literal text or path to a file)
        #[arg(long)]
        context: Option<String>,
        /// Migrate Claude Code sessions from this old path
        #[arg(long)]
        from: Option<PathBuf>,
        /// Don't auto-accept edits in the spawned Claude session
        #[arg(long)]
        no_edit: bool,
        /// Agent to spawn (overrides project default)
        #[arg(long)]
        agent: Option<String>,
    },
    /// List all features with their status
    List,
    /// Show detailed info for a feature
    Info {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
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
        /// Keep the feature after merge (preserve session, worktree, branch, and state)
        #[arg(long)]
        keep: bool,
    },
    /// Create or link a GitHub PR for a feature
    Pr {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
        /// Create a non-draft (ready) PR instead of draft
        #[arg(long)]
        ready: bool,
        /// PR body (literal text or path to a file; overrides template)
        #[arg(long)]
        body: Option<String>,
    },
    /// Mark a feature's PR as ready for review
    Ready {
        /// Feature name (detected from CWD if omitted)
        name: Option<String>,
    },
    /// Rename a feature (branch, worktree, tmux session, state)
    Rename {
        /// Current feature name (detected from CWD if omitted)
        old_name: Option<String>,
        /// New feature name
        new_name: String,
    },
    /// Check out a PR for review (fetch branch + worktree + tmux session)
    Review {
        /// PR number or GitHub PR URL
        pr: String,
    },
    /// Sync feature statuses with their linked GitHub PRs
    Sync {
        /// Feature name (syncs just this feature). Detected from CWD if omitted. If not in a feature worktree, syncs all features.
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

/// Resolve the current scope: feature name if in a feature worktree,
/// "main" if in the main worktree, error otherwise.
fn resolve_scope(project_root: &std::path::Path) -> pm::error::Result<String> {
    resolve_scope_from(project_root, &std::env::current_dir()?)
}

fn resolve_scope_from(
    project_root: &std::path::Path,
    cwd: &std::path::Path,
) -> pm::error::Result<String> {
    if let Some(feature) = paths::detect_feature_from_cwd(project_root, cwd) {
        return Ok(feature);
    }
    if paths::is_in_main_worktree(project_root, cwd) {
        return Ok("main".to_string());
    }
    Err(PmError::NotInWorktree)
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
            commands::register::register(&path, name.as_deref(), &projects_dir, r#move, None, None)
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
        Commands::Claude(claude_cmd) => match claude_cmd {
            ClaudeCommands::Settings(settings_cmd) => {
                let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                match settings_cmd {
                    ClaudeSettingsCommands::List { name } => {
                        let scope = match name {
                            Some(n) => n,
                            None => resolve_scope(&project_root)?,
                        };
                        let (label, lines) = if scope == "main" {
                            (
                                "main".to_string(),
                                commands::claude_settings::list_main(&project_root)?,
                            )
                        } else {
                            let lines = commands::claude_settings::list(&project_root, &scope)?;
                            (scope, lines)
                        };
                        if lines.is_empty() {
                            println!("No settings files found for '{label}'");
                        } else {
                            for line in lines {
                                println!("{line}");
                            }
                        }
                        Ok(())
                    }
                    ClaudeSettingsCommands::Push { name } => {
                        let name = resolve_feature_name(name, &project_root)?;
                        commands::claude_settings::push(&project_root, &name)?;
                        println!("Pushed settings from feature '{name}' to main");
                        Ok(())
                    }
                    ClaudeSettingsCommands::Pull { name } => {
                        let name = resolve_feature_name(name, &project_root)?;
                        commands::claude_settings::pull(&project_root, &name)?;
                        println!("Pulled settings from main into feature '{name}'");
                        Ok(())
                    }
                    ClaudeSettingsCommands::Diff { name } => {
                        let name = resolve_feature_name(name, &project_root)?;
                        let lines = commands::claude_settings::diff(&project_root, &name)?;
                        if lines.is_empty() {
                            println!("No differences");
                        } else {
                            for line in lines {
                                println!("{line}");
                            }
                        }
                        Ok(())
                    }
                    ClaudeSettingsCommands::Merge { name, ours } => {
                        let name = resolve_feature_name(name, &project_root)?;
                        commands::claude_settings::merge(&project_root, &name, ours)?;
                        println!("Merged settings from feature '{name}' into main");
                        Ok(())
                    }
                }
            }
            ClaudeCommands::Skills(skills_cmd) => match skills_cmd {
                ClaudeSkillsCommands::List => {
                    let project_root = paths::find_project_root(&std::env::current_dir()?).ok();
                    let lines = commands::skills::skills_list(project_root.as_deref())?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                ClaudeSkillsCommands::Install { name, global } => {
                    let messages = if global {
                        commands::skills::skills_install(name.as_deref())?
                    } else {
                        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                        commands::skills::skills_install_project(&project_root, name.as_deref())?
                    };
                    for msg in messages {
                        println!("{msg}");
                    }
                    Ok(())
                }
                ClaudeSkillsCommands::Uninstall { name, all, global } => {
                    if name.is_none() && !all {
                        eprintln!("Provide a skill name or use --all to uninstall all");
                        std::process::exit(1);
                    }
                    let messages = if global {
                        commands::skills::skills_uninstall(name.as_deref())?
                    } else {
                        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                        commands::skills::skills_uninstall_project(&project_root, name.as_deref())?
                    };
                    for msg in messages {
                        println!("{msg}");
                    }
                    Ok(())
                }
                ClaudeSkillsCommands::Pull { name } => {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::skills::skills_pull(&project_root, &name)?;
                    println!("Pulled skills from main into feature '{name}'");
                    Ok(())
                }
            },
            ClaudeCommands::Agents(agents_cmd) => match agents_cmd {
                ClaudeAgentsCommands::List => {
                    let project_root = paths::find_project_root(&std::env::current_dir()?).ok();
                    let lines = commands::skills::agents_list(project_root.as_deref())?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                ClaudeAgentsCommands::Install { name, global } => {
                    let messages = if global {
                        commands::skills::agents_install(name.as_deref())?
                    } else {
                        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                        commands::skills::agents_install_project(&project_root, name.as_deref())?
                    };
                    for msg in messages {
                        println!("{msg}");
                    }
                    Ok(())
                }
                ClaudeAgentsCommands::Uninstall { name, all, global } => {
                    if name.is_none() && !all {
                        eprintln!("Provide an agent name or use --all to uninstall all");
                        std::process::exit(1);
                    }
                    let messages = if global {
                        commands::skills::agents_uninstall(name.as_deref())?
                    } else {
                        let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                        commands::skills::agents_uninstall_project(&project_root, name.as_deref())?
                    };
                    for msg in messages {
                        println!("{msg}");
                    }
                    Ok(())
                }
            },
            ClaudeCommands::Hooks(hooks_cmd) => match hooks_cmd {
                HooksCommands::Install => {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    let msg = commands::hooks_install::install(&project_root)?;
                    println!("{msg}");
                    Ok(())
                }
                HooksCommands::Stop => {
                    let code = commands::hooks_stop::stop();
                    if code != 0 {
                        std::process::exit(code);
                    }
                    Ok(())
                }
            },
            ClaudeCommands::Migrate { from } => {
                let cwd = std::env::current_dir()?;
                let messages = commands::claude_migrate::migrate_sessions(&from, &cwd, None)?;
                for msg in messages {
                    println!("{msg}");
                }
                Ok(())
            }
        },
        Commands::Agent(agent_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            let feature = resolve_scope(&project_root)?;
            match agent_cmd {
                AgentCommands::Spawn {
                    name,
                    context,
                    edit,
                } => {
                    if let Some(agent_name) = name {
                        let msg = commands::agent_spawn::agent_spawn(
                            &project_root,
                            &feature,
                            &agent_name,
                            context.as_deref(),
                            edit,
                            None,
                        )?;
                        println!("{msg}");
                    } else {
                        let msgs =
                            commands::agent_spawn::agent_spawn_all(&project_root, &feature, None)?;
                        for msg in msgs {
                            println!("{msg}");
                        }
                    }
                    Ok(())
                }
                AgentCommands::List { active } => {
                    let lines = commands::agent_list::agent_list(&project_root, &feature, active)?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
            }
        }
        Commands::Msg(msg_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            let feature = resolve_scope(&project_root)?;
            match msg_cmd {
                MsgCommands::Send {
                    agent,
                    message,
                    as_agent,
                } => {
                    let sender = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let line = commands::agent_send::agent_send(
                        &project_root,
                        &feature,
                        &agent,
                        &sender,
                        &message,
                        None,
                    )?;
                    println!("{line}");
                    Ok(())
                }
                MsgCommands::Read {
                    from,
                    index,
                    as_agent,
                } => {
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let spec = index
                        .as_deref()
                        .map(commands::agent_read::IndexSpec::parse)
                        .transpose()?;
                    let lines = commands::agent_read::agent_read(
                        &project_root,
                        &feature,
                        &agent,
                        from.as_deref(),
                        spec,
                    )?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                MsgCommands::Next { from, as_agent } => {
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let line = commands::agent_next::agent_next(
                        &project_root,
                        &feature,
                        &agent,
                        from.as_deref(),
                    )?;
                    println!("{line}");
                    Ok(())
                }
                MsgCommands::List { from, as_agent } => {
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let lines = commands::msg_list::msg_list(
                        &project_root,
                        &feature,
                        &agent,
                        from.as_deref(),
                    )?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                MsgCommands::Wait { from, as_agent } => {
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let count = commands::agent_wait::agent_wait(
                        &project_root,
                        &feature,
                        &agent,
                        from.as_deref(),
                        None,
                    )?;
                    println!("{count} new message{}", if count == 1 { "" } else { "s" });
                    Ok(())
                }
            }
        }
        Commands::Feat(feat_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match feat_cmd {
                FeatCommands::New {
                    name,
                    feature_name,
                    context,
                    base,
                    no_edit,
                    agent,
                } => {
                    let feat_name = commands::feat_new::feat_new(
                        &project_root,
                        &name,
                        feature_name.as_deref(),
                        context.as_deref(),
                        base.as_deref(),
                        no_edit,
                        agent.as_deref(),
                        None,
                    )?;
                    println!("Created feature '{feat_name}'");
                    Ok(())
                }
                FeatCommands::Adopt {
                    name,
                    feature_name,
                    context,
                    from,
                    no_edit,
                    agent,
                } => {
                    let feat_name = commands::feat_adopt::feat_adopt(
                        &project_root,
                        &name,
                        feature_name.as_deref(),
                        context.as_deref(),
                        from.as_deref(),
                        no_edit,
                        agent.as_deref(),
                        None,
                        None,
                    )?;
                    println!("Adopted feature '{feat_name}'");
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
                FeatCommands::Info { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    let lines = commands::feat_info::feat_info(&project_root, &name)?;
                    for line in lines {
                        println!("{line}");
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
                FeatCommands::Merge { name, keep } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::feat_merge::feat_merge(&project_root, &name, keep, None)?;
                    if keep {
                        println!("Merged feature '{name}'");
                    } else {
                        println!("Merged and deleted feature '{name}'");
                    }
                    Ok(())
                }
                FeatCommands::Pr { name, ready, body } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    let resolved_body = body
                        .as_deref()
                        .map(commands::feat_new::resolve_context)
                        .transpose()?;
                    commands::feat_pr::feat_pr(
                        &project_root,
                        &name,
                        ready,
                        resolved_body.as_deref(),
                    )?;
                    println!("PR linked for feature '{name}'");
                    Ok(())
                }
                FeatCommands::Ready { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::feat_ready::feat_ready(&project_root, &name)?;
                    println!("PR marked ready for feature '{name}'");
                    Ok(())
                }
                FeatCommands::Rename { old_name, new_name } => {
                    let old_name = resolve_feature_name(old_name, &project_root)?;
                    commands::feat_rename::feat_rename(&project_root, &old_name, &new_name, None)?;
                    println!("Renamed feature '{old_name}' to '{new_name}'");
                    Ok(())
                }
                FeatCommands::Review { pr } => {
                    let feature_name =
                        commands::feat_review::feat_review(&project_root, &pr, None)?;
                    println!("Created review feature '{feature_name}'");
                    Ok(())
                }
                FeatCommands::Sync { name } => {
                    let name = name.or_else(|| {
                        paths::detect_feature_from_cwd(
                            &project_root,
                            &std::env::current_dir().ok()?,
                        )
                    });
                    let messages = commands::feat_sync::feat_sync(&project_root, name.as_deref())?;
                    for msg in messages {
                        println!("{msg}");
                    }
                    Ok(())
                }
            }
        }
        Commands::Delete {
            project,
            force,
            yes,
        } => {
            let projects_dir = paths::global_projects_dir()?;
            let project_root = if let Some(name) = &project {
                let entry = pm::state::project::ProjectEntry::load(&projects_dir, name)?;
                PathBuf::from(&entry.root)
            } else {
                paths::find_project_root(&std::env::current_dir()?)?
            };
            let project_name =
                commands::delete::delete(&project_root, &projects_dir, force, yes, None)?;
            println!("Deleted project '{project_name}'");
            Ok(())
        }
        Commands::Status { project } => {
            let project_root = if let Some(name) = project {
                let projects_dir = paths::global_projects_dir()?;
                let entry = pm::state::project::ProjectEntry::load(&projects_dir, &name)?;
                PathBuf::from(&entry.root)
            } else {
                paths::find_project_root(&std::env::current_dir()?)?
            };
            let lines = commands::status::status(&project_root, None)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::Doctor { fix, project } => {
            let project_root = if let Some(name) = project {
                let projects_dir = paths::global_projects_dir()?;
                let entry = pm::state::project::ProjectEntry::load(&projects_dir, &name)?;
                PathBuf::from(&entry.root)
            } else {
                paths::find_project_root(&std::env::current_dir()?)?
            };
            let lines = commands::doctor::doctor(&project_root, fix, None)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_feature_state(root: &std::path::Path, name: &str) {
        let feat_dir = root.join(".pm").join("features");
        std::fs::create_dir_all(&feat_dir).unwrap();
        std::fs::write(feat_dir.join(format!("{name}.toml")), "").unwrap();
    }

    #[test]
    fn scope_returns_feature_name_in_feature_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        create_feature_state(root, "login");
        let cwd = root.join("login").join("src");
        std::fs::create_dir_all(&cwd).unwrap();

        let scope = resolve_scope_from(root, &cwd).unwrap();
        assert_eq!(scope, "login");
    }

    #[test]
    fn scope_returns_main_in_main_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();
        let cwd = root.join("main").join("src");
        std::fs::create_dir_all(&cwd).unwrap();

        let scope = resolve_scope_from(root, &cwd).unwrap();
        assert_eq!(scope, "main");
    }

    #[test]
    fn scope_errors_outside_worktree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = resolve_scope_from(root, root);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::NotInWorktree));
    }
}
