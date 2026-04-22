use clap::CommandFactory;

use crate::cli::*;
use pm::commands;
use pm::error::PmError;
use pm::state::paths;
use pm::tmux;

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

/// Validate that a scope name refers to an existing scope ("main" or a known feature).
fn validate_scope(project_root: &std::path::Path, scope: &str) -> pm::error::Result<()> {
    if scope == "main" {
        return Ok(());
    }
    let features_dir = paths::features_dir(project_root);
    let state_file = features_dir.join(format!("{scope}.toml"));
    if state_file.exists() {
        Ok(())
    } else {
        Err(PmError::FeatureNotFound(scope.to_string()))
    }
}

/// Resolve scope with an optional override flag. If `scope_flag` is Some,
/// validates it and returns it; otherwise auto-detects from CWD.
fn resolve_scope_with_flag(
    project_root: &std::path::Path,
    scope_flag: Option<String>,
) -> pm::error::Result<String> {
    match scope_flag {
        Some(s) => {
            validate_scope(project_root, &s)?;
            Ok(s)
        }
        None => resolve_scope(project_root),
    }
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

pub fn run(cli: Cli) -> pm::error::Result<()> {
    match cli.command {
        Commands::Init { path, git } => {
            let projects_dir = paths::global_projects_dir()?;
            commands::init::init(&path, &projects_dir, git.as_deref(), None)
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
            let result = commands::open::open(&project_root, None)?;
            if result.sessions_restored == 0 && result.agents_respawned == 0 {
                println!("Project sessions opened");
            } else {
                println!(
                    "Restored {} sessions. Respawned {} agents.",
                    result.sessions_restored, result.agents_respawned
                );
            }
            Ok(())
        }
        Commands::Close => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            let (project_name, killed) = commands::close::close(&project_root, None)?;
            println!(
                "Closed project {project_name} (killed {killed} session{})",
                if killed == 1 { "" } else { "s" }
            );
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
                HooksCommands::SessionStart => {
                    let code = commands::hooks_session_start::session_start();
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
            ClaudeCommands::Export { all, output } => {
                let projects_dir = paths::global_projects_dir()?;
                let project_root = if all {
                    None
                } else {
                    Some(paths::find_project_root(&std::env::current_dir()?)?)
                };
                let (_, messages) = commands::claude_export::export(
                    project_root.as_deref(),
                    &projects_dir,
                    all,
                    output.as_deref(),
                    None,
                )?;
                for msg in messages {
                    println!("{msg}");
                }
                Ok(())
            }
            ClaudeCommands::Import { tarball } => {
                let projects_dir = paths::global_projects_dir()?;
                let messages = commands::claude_import::import(&tarball, &projects_dir, None)?;
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
                        let result =
                            commands::agent_spawn::agent_spawn_all(&project_root, &feature, None)?;
                        for msg in &result.successes {
                            println!("{msg}");
                        }
                        for err in &result.errors {
                            eprintln!("error: {err}");
                        }
                    }
                    Ok(())
                }
                AgentCommands::Stop { name, scope } => {
                    let target_scope = scope.unwrap_or(feature);
                    let msg = commands::agent_stop::agent_stop(
                        &project_root,
                        &target_scope,
                        &name,
                        None,
                    )?;
                    println!("{msg}");
                    Ok(())
                }
                AgentCommands::List { active } => {
                    let lines =
                        commands::agent_list::agent_list(&project_root, &feature, active, None)?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                AgentCommands::Check { name } => {
                    let sender = pm::messages::default_user_name();
                    if let Some(agent_name) = name {
                        let msg = commands::agent_check::agent_check(
                            &project_root,
                            &feature,
                            &agent_name,
                            &sender,
                            None,
                        )?;
                        println!("{msg}");
                    } else {
                        let (successes, errors) = commands::agent_check::agent_check_all(
                            &project_root,
                            &feature,
                            &sender,
                            None,
                        )?;
                        for msg in &successes {
                            println!("{msg}");
                        }
                        for err in &errors {
                            eprintln!("error: {err}");
                        }
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
                    scope,
                    upstream,
                    project: target_project,
                } => {
                    let sender = as_agent.unwrap_or_else(pm::messages::default_user_name);

                    if let Some(ref proj_name) = target_project {
                        // Cross-project delivery: resolve target project root,
                        // deliver message, but do NOT auto-spawn.
                        let target_scope = scope.as_deref().unwrap_or("main");
                        let pm_dir = paths::pm_dir(&project_root);
                        let sender_project_config =
                            pm::state::project::ProjectConfig::load(&pm_dir)?;
                        let sender_project_name = &sender_project_config.project.name;
                        let line = commands::agent_send::agent_send_cross_project(
                            &commands::agent_send::CrossProjectSendParams {
                                target_project_name: proj_name,
                                sender_scope: &feature,
                                sender_project: sender_project_name,
                                target_scope,
                                recipient: &agent,
                                sender: &sender,
                                body: &message,
                            },
                        )?;
                        println!("{line}");
                    } else {
                        let target_scope = if upstream {
                            Some(commands::agent_send::resolve_upstream(
                                &project_root,
                                &feature,
                            )?)
                        } else {
                            scope
                        };
                        let line = commands::agent_send::agent_send(
                            &project_root,
                            &feature,
                            target_scope.as_deref(),
                            &agent,
                            &sender,
                            &message,
                            None,
                        )?;
                        println!("{line}");
                    }
                    Ok(())
                }
                MsgCommands::Read {
                    from,
                    index,
                    as_agent,
                    scope,
                } => {
                    let target_scope = resolve_scope_with_flag(&project_root, scope)?;
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let spec = index
                        .as_deref()
                        .map(commands::agent_read::IndexSpec::parse)
                        .transpose()?;
                    let lines = commands::agent_read::agent_read(
                        &project_root,
                        &target_scope,
                        &agent,
                        from.as_deref(),
                        spec,
                    )?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                MsgCommands::List {
                    from,
                    as_agent,
                    scope,
                } => {
                    let target_scope = resolve_scope_with_flag(&project_root, scope)?;
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let lines = commands::msg_list::msg_list(
                        &project_root,
                        &target_scope,
                        &agent,
                        from.as_deref(),
                    )?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                MsgCommands::Wait {
                    from,
                    as_agent,
                    scope,
                } => {
                    let target_scope = resolve_scope_with_flag(&project_root, scope)?;
                    let agent = as_agent.unwrap_or_else(pm::messages::default_user_name);
                    let count = commands::agent_wait::agent_wait(
                        &project_root,
                        &target_scope,
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
                    edit,
                    agent,
                } => {
                    let feat_name =
                        commands::feat_new::feat_new(&commands::feat_new::FeatNewParams {
                            project_root: &project_root,
                            name: &name,
                            name_override: feature_name.as_deref(),
                            context: context.as_deref(),
                            base: base.as_deref(),
                            edit,
                            agent_override: agent.as_deref(),
                            tmux_server: None,
                        })?;
                    println!("Created feature '{feat_name}'");
                    Ok(())
                }
                FeatCommands::Adopt {
                    name,
                    feature_name,
                    context,
                    from,
                    edit,
                    agent,
                } => {
                    let feat_name =
                        commands::feat_adopt::feat_adopt(&commands::feat_adopt::FeatAdoptParams {
                            project_root: &project_root,
                            name: &name,
                            name_override: feature_name.as_deref(),
                            context: context.as_deref(),
                            from: from.as_deref(),
                            edit,
                            agent_override: agent.as_deref(),
                            tmux_server: None,
                            claude_base: None,
                        })?;
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
                entry.root_path()
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
                entry.root_path()
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
                entry.root_path()
            } else {
                paths::find_project_root(&std::env::current_dir()?)?
            };
            let lines = commands::doctor::doctor(&project_root, fix, None)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::Upgrade { all } => {
            let lines = commands::upgrade::upgrade(all)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::Restore => {
            let messages = commands::restore::restore(None)?;
            for msg in messages {
                println!("{msg}");
            }
            Ok(())
        }
        Commands::State(state_cmd) => match state_cmd {
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
        },
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "pm", &mut std::io::stdout());
            Ok(())
        }
        Commands::Summary { command } => match command {
            SummaryCommands::Write { content } => {
                let path = std::path::Path::new(&content);
                let body = if path.exists() {
                    std::fs::read_to_string(path)?
                } else {
                    content
                };
                commands::summary::run(&body)
            }
        },
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
        let cwd = paths::main_worktree(root).join("src");
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

    #[test]
    fn validate_scope_accepts_main() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        assert!(validate_scope(root, "main").is_ok());
    }

    #[test]
    fn validate_scope_accepts_existing_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");

        assert!(validate_scope(root, "login").is_ok());
    }

    #[test]
    fn validate_scope_rejects_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = validate_scope(root, "nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn resolve_scope_with_flag_uses_override() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");

        let scope = resolve_scope_with_flag(root, Some("login".to_string())).unwrap();
        assert_eq!(scope, "login");
    }

    #[test]
    fn resolve_scope_with_flag_rejects_invalid_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = resolve_scope_with_flag(root, Some("bogus".to_string()));
        assert!(result.is_err());
    }
}
