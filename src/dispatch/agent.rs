use pm::commands;
use pm::error::{PmError, Result};
use pm::state::paths;

use super::report::{report_agent_op_results, report_restart_all, restart_select};
use super::scope::{project_root, resolve_scope_with_flag};
use super::window::push;
use crate::cli::AgentCommands;

pub(super) fn run(cmd: AgentCommands, server: Option<&str>) -> Result<()> {
    if let AgentCommands::Restart {
        all: true,
        global: true,
        stale,
        force,
        ..
    } = cmd
    {
        let (scopes, unread) =
            commands::agent_restart_all::global_scopes(&paths::global_projects_dir()?)?;
        return report_restart_all(&scopes, unread, restart_select(stale), force, server);
    }
    let project_root = project_root(&paths::global_projects_dir()?, project(&cmd))?;
    match cmd {
        AgentCommands::Spawn {
            name,
            agent_definition,
            context,
            scope,
            ..
        } => {
            let feature = resolve_scope_with_flag(&project_root, scope)?;
            // `--context -` reads the brief from stdin; any other
            // value is treated as a literal string (no file resolution).
            let context = commands::feat_new::resolve_stdin_context(context.as_deref())?;
            if let Some(agent_name) = name {
                let (outcome, msg, _) = commands::agent_spawn::agent_spawn(
                    &project_root,
                    &feature,
                    &agent_name,
                    agent_definition.as_deref(),
                    context.as_deref(),
                    server,
                )?;
                if outcome.is_new_window()
                    && let Err(e) =
                        commands::launch_check::check(&project_root, &feature, &agent_name, server)
                {
                    push();
                    return Err(e);
                }
                println!("{msg}");
            } else {
                if agent_definition.is_some() {
                    return Err(PmError::Agent(
                        "--agent requires a positional NAME (the display name to register under)"
                            .to_string(),
                    ));
                }
                let mut result =
                    commands::agent_spawn::agent_spawn_all(&project_root, &feature, server)?;
                result.confirm_launches(&project_root, &feature, server);
                for msg in &result.successes {
                    println!("{msg}");
                }
                for err in &result.errors {
                    eprintln!("error: {err}");
                }
            }
            push();
            Ok(())
        }
        AgentCommands::Stop { names, scope, .. } => {
            let target_scope = resolve_scope_with_flag(&project_root, scope)?;
            let results =
                commands::agent_stop::agent_stop_many(&project_root, &target_scope, &names, server);
            push();
            report_agent_op_results(results, "stop")
        }
        AgentCommands::Delete { names, scope, .. } => {
            let target_scope = resolve_scope_with_flag(&project_root, scope)?;
            let results = commands::agent_delete::agent_delete_many(
                &project_root,
                &target_scope,
                &names,
                server,
            );
            push();
            report_agent_op_results(results, "delete")
        }
        AgentCommands::Restart {
            names,
            all,
            stale,
            force,
            scope,
            ..
        } => {
            let target_scope = resolve_scope_with_flag(&project_root, scope)?;
            if all {
                let scope = commands::agent_restart_all::Scope::of(&project_root, &target_scope)?;
                return report_restart_all(
                    &[scope],
                    Vec::new(),
                    restart_select(stale),
                    force,
                    server,
                );
            }
            let mut restarted = commands::agent_restart::agent_restart_many(
                &project_root,
                &target_scope,
                &names,
                force,
                true,
                server,
            );
            restarted.confirm_launches(&project_root, &target_scope, server);
            let failure = restarted.not_up_failure();
            let reported =
                report_agent_op_results(std::mem::take(&mut restarted.results), "restart")
                    .map_err(|e| failure.map_or(e, |f| PmError::Agent(f.to_string())));
            std::io::Write::flush(&mut std::io::stdout())?;
            restarted.finish(server);
            push();
            reported
        }
        AgentCommands::List { active, scope, .. } => {
            let feature = resolve_scope_with_flag(&project_root, scope)?;
            let lines = commands::agent_list::agent_list(&project_root, &feature, active)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        AgentCommands::Log {
            name, lines, scope, ..
        } => {
            let scope = resolve_scope_with_flag(&project_root, scope)?;
            print!(
                "{}",
                commands::agent_log::log(&project_root, &scope, &name, usize::from(lines))?
            );
            Ok(())
        }
        AgentCommands::Mode {
            name, mode, scope, ..
        } => {
            let scope = resolve_scope_with_flag(&project_root, scope)?;
            print!(
                "{}",
                commands::agent_mode::run(&project_root, &scope, &name, mode.as_deref(), server)?
            );
            Ok(())
        }
        AgentCommands::Fork {
            source,
            name,
            scope,
            ..
        } => {
            let feature = resolve_scope_with_flag(&project_root, scope)?;
            let msg =
                commands::agent_fork::agent_fork(&project_root, &feature, &source, &name, server)?;
            let launched = commands::launch_check::check(&project_root, &feature, &name, server);
            push();
            launched?;
            println!("{msg}");
            Ok(())
        }
    }
}

/// The project `--project` names, if any.
fn project(cmd: &AgentCommands) -> Option<&str> {
    match cmd {
        AgentCommands::Spawn { target, .. }
        | AgentCommands::List { target, .. }
        | AgentCommands::Stop { target, .. }
        | AgentCommands::Delete { target, .. }
        | AgentCommands::Restart { target, .. }
        | AgentCommands::Fork { target, .. }
        | AgentCommands::Mode { target, .. }
        | AgentCommands::Log { target, .. } => target.project.as_deref(),
    }
}
