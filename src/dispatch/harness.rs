use pm::commands;
use pm::commands::attention::AgentState;
use pm::commands::harness_export::ExportParams;
use pm::commands::harness_migrate::MigrateParams;
use pm::error::Result;
use pm::harness::Harness;
use pm::state::paths;
use pm::state::project::GlobalConfig;

use super::scope::{optional_project_root, resolve_feature_name, resolve_scope};
use super::window::{agent_window, publish, push, running_agent, tmux_server_from_env};
use crate::cli::*;

pub(super) fn run(cmd: HarnessCommands) -> Result<()> {
    match cmd {
        HarnessCommands::Settings { harness, command } => {
            commands::harness_settings::settings_files(harness)?;
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match command {
                HarnessSettingsCommands::List { name } => {
                    let scope = match name {
                        Some(n) => n,
                        None => resolve_scope(&project_root)?,
                    };
                    let (label, lines) = if scope == "main" {
                        (
                            "main".to_string(),
                            commands::harness_settings::list_main(&project_root, harness)?,
                        )
                    } else {
                        let lines =
                            commands::harness_settings::list(&project_root, &scope, harness)?;
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
                HarnessSettingsCommands::Push { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::harness_settings::push(&project_root, &name, harness)?;
                    println!("Pushed settings from feature '{name}' to main");
                    Ok(())
                }
                HarnessSettingsCommands::Pull { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    for line in commands::harness_settings::pull(&project_root, &name, harness)? {
                        println!("{line}");
                    }
                    Ok(())
                }
                HarnessSettingsCommands::Diff { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    let lines = commands::harness_settings::diff(&project_root, &name, harness)?;
                    if lines.is_empty() {
                        println!("No differences");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Ok(())
                }
                HarnessSettingsCommands::Merge { name, ours } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::harness_settings::merge(&project_root, &name, ours, harness)?;
                    println!("Merged settings from feature '{name}' into main");
                    Ok(())
                }
            }
        }
        HarnessCommands::Skills(skills_cmd) => match skills_cmd {
            HarnessSkillsCommands::List => {
                let project_root = optional_project_root()?;
                let lines = commands::skills::skills_list(project_root.as_deref())?;
                for line in lines {
                    println!("{line}");
                }
                Ok(())
            }
            HarnessSkillsCommands::Install { name } => {
                for msg in commands::skills::skills_install(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
            HarnessSkillsCommands::Uninstall { name, all } => {
                if name.is_none() && !all {
                    eprintln!("Provide a skill name or use --all to uninstall all");
                    std::process::exit(1);
                }
                for msg in commands::skills::skills_uninstall(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
        },
        HarnessCommands::Agents(agents_cmd) => match agents_cmd {
            HarnessAgentsCommands::List => {
                let project_root = optional_project_root()?;
                let lines = commands::skills::agents_list(project_root.as_deref())?;
                for line in lines {
                    println!("{line}");
                }
                Ok(())
            }
            HarnessAgentsCommands::Install { name } => {
                for msg in commands::skills::agents_install(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
            HarnessAgentsCommands::Uninstall { name, all } => {
                if name.is_none() && !all {
                    eprintln!("Provide an agent name or use --all to uninstall all");
                    std::process::exit(1);
                }
                for msg in commands::skills::agents_uninstall(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
        },
        HarnessCommands::Hooks(hooks_cmd) => match hooks_cmd {
            HarnessHooksCommands::Install => {
                let project_root = optional_project_root()?;
                let msg = commands::hooks_install::install(project_root.as_deref())?;
                println!("{msg}");
                Ok(())
            }
            HarnessHooksCommands::Stop { harness } => {
                let mut window = running_agent()
                    .and_then(|agent| agent_window(tmux_server_from_env().as_deref(), &agent));
                exit_unless_ok(commands::hooks_stop::stop(harness, &mut |state, unread| {
                    if let Some(window) = window.as_mut() {
                        window.publish(state, unread);
                    }
                    if state != AgentState::Busy {
                        push();
                    }
                }))
            }
            HarnessHooksCommands::SessionStart => {
                let code = commands::hooks_session_start::session_start();
                // The harness is running now, so its window no longer reads
                // as dead.
                if running_agent().is_some() {
                    push();
                }
                exit_unless_ok(code)
            }
            HarnessHooksCommands::UserPrompt => {
                exit_unless_ok(commands::hooks_user_prompt::user_prompt(|unread| {
                    publish(AgentState::Busy, unread);
                    push();
                }))
            }
            HarnessHooksCommands::Waiting { harness } => exit_unless_ok(
                commands::hooks_waiting::waiting(harness, |state, unread| {
                    publish(state, unread);
                    push();
                }),
            ),
            HarnessHooksCommands::Dialog { harness } => {
                exit_unless_ok(commands::hooks_dialog::dialog(harness, push))
            }
        },
        HarnessCommands::Pull { name, dry_run } => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            let name = resolve_feature_name(name, &project_root)?;
            let pulled = commands::seed::pull(&project_root, &name, dry_run)?;
            if pulled.written.is_empty() && pulled.removed.is_empty() {
                println!("Feature '{name}' is up to date with main");
            }
            let verb = if dry_run { "Would write" } else { "Wrote" };
            for file in pulled.written {
                println!("{verb} {name}/{}", file.display());
            }
            let verb = if dry_run { "Would remove" } else { "Removed" };
            for file in pulled.removed {
                println!(
                    "{verb} {name}/{}: the branch deleted its skill",
                    file.display()
                );
            }
            if dry_run {
                for file in pulled.deleted {
                    println!(
                        "Would skip {name}/{}: the branch deleted it",
                        file.display()
                    );
                }
            }
            Ok(())
        }
        HarnessCommands::Migrate { from, harness } => {
            let cwd = std::env::current_dir()?;
            let project_root = optional_project_root()?;
            let messages = commands::harness_migrate::migrate(&MigrateParams {
                harness,
                from: &from,
                to: &cwd,
                project_root: project_root.as_deref(),
                home: &paths::home_dir()?,
                global: &GlobalConfig::load_or_default().harness,
                tmux_server: tmux_server_from_env().as_deref(),
            })?;
            for msg in messages {
                println!("{msg}");
            }
            Ok(())
        }
        HarnessCommands::Export {
            all,
            output,
            harness,
        } => {
            let project_root = if all {
                None
            } else {
                Some(paths::find_project_root(&std::env::current_dir()?)?)
            };
            let (_, messages) = commands::harness_export::export(&ExportParams {
                harness,
                project_root: project_root.as_deref(),
                projects_dir: &paths::global_projects_dir()?,
                all,
                output: output.as_deref(),
                home: &paths::home_dir()?,
                global: &GlobalConfig::load_or_default().harness,
            })?;
            for msg in messages {
                println!("{msg}");
            }
            Ok(())
        }
        HarnessCommands::Import { tarball, harness } => {
            let report = commands::harness_import::import(
                harness,
                &tarball,
                &[],
                &paths::global_projects_dir()?,
                &paths::home_dir()?,
                &GlobalConfig::load_or_default().harness,
            )?;
            for msg in report.messages {
                println!("{msg}");
            }
            Ok(())
        }
        HarnessCommands::List => {
            for h in Harness::SUPPORTED {
                if *h == Harness::default() {
                    println!("{h} (default)");
                } else {
                    println!("{h}");
                }
            }
            Ok(())
        }
        HarnessCommands::Probe { harness } => {
            println!(
                "{}",
                commands::doctor::probe_line(harness, optional_project_root()?.as_deref())
            );
            Ok(())
        }
    }
}

/// Hook handlers hand back a process exit code; a non-zero one is the
/// handler's whole answer to the harness and must reach it verbatim.
fn exit_unless_ok(code: i32) -> Result<()> {
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
