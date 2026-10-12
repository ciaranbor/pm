use pm::commands;
use pm::commands::agent_send::{Home, Sender};
use pm::commands::attention::AgentState;
use pm::error::{PmError, Result};
use pm::state::paths;
use pm::state::project::ProjectEntry;
use pm::state::scope;
use std::path::PathBuf;

use super::report::report_sent;
use super::scope::{
    optional_agent_scope, own_scope, project_root, resolve_scope, resolve_scope_with_flag,
};
use super::window::{agent_window, push, running_agent};
use crate::cli::MsgCommands;

pub(super) fn run(cmd: MsgCommands, server: Option<&str>) -> Result<()> {
    match cmd {
        MsgCommands::Send {
            agent,
            message,
            scope,
            upstream,
            project: target_project,
            delay,
        } => {
            let message = read_message_body(message)?;
            let sender = pm::messages::default_user_name();

            // Parse agent@scope shorthand
            let (recipient, shorthand_scope) = parse_agent_at_scope(&agent);
            if shorthand_scope.is_some() && scope.is_some() {
                return Err(PmError::Messaging(
                    "Cannot use both agent@scope shorthand and --scope flag".to_string(),
                ));
            }
            if shorthand_scope.is_some() && upstream {
                return Err(PmError::Messaging(
                    "Cannot use both agent@scope shorthand and --upstream flag".to_string(),
                ));
            }
            let effective_scope = shorthand_scope.map(|s| s.to_string()).or(scope);

            // The sender's home, if it has one, is where a reply comes back.
            let (target_root, target_scope, home) = if let Some(ref proj_name) = target_project {
                let home = optional_agent_scope()?;
                let (_, target_root) =
                    ProjectEntry::load_here(&paths::global_projects_dir()?, proj_name)?;
                let target_scope = effective_scope
                    .or_else(|| own_scope(&target_root))
                    .unwrap_or_else(|| scope::MAIN.to_string());
                (target_root, target_scope, home)
            } else {
                let (project_root, feature) = paths::agent_scope()?;
                let target_scope = if upstream {
                    let main_branch =
                        ProjectEntry::main_branch(&project_root, &paths::global_projects_dir()?)?;
                    commands::agent_send::resolve_upstream(&project_root, &main_branch, &feature)?
                } else {
                    effective_scope.unwrap_or_else(|| feature.clone())
                };
                (
                    project_root.clone(),
                    target_scope,
                    Some((project_root, feature)),
                )
            };
            let sender = Sender {
                name: &sender,
                home: home.as_ref().map(|(root, scope)| Home { root, scope }),
            };
            if let Some(delay) = delay {
                let send = commands::msg_scheduled::schedule(
                    &target_root,
                    &target_scope,
                    recipient,
                    sender,
                    &message,
                    delay,
                )?;
                println!("{}", commands::msg_scheduled::describe_scheduled(&send));
                return Ok(());
            }
            let sent = commands::agent_send::send_as(
                &target_root,
                &target_scope,
                recipient,
                sender,
                &message,
                server,
            )?;
            report_sent(sent, server);
            push();
            Ok(())
        }
        MsgCommands::Scheduled { target } => {
            let (project_root, _) = target_project(target.project.as_deref())?;
            let lines = commands::msg_scheduled::list(&project_root)?;
            if lines.is_empty() {
                println!("No scheduled sends");
            }
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        MsgCommands::Cancel { id, target } => {
            let (project_root, _) = target_project(target.project.as_deref())?;
            println!("{}", commands::msg_scheduled::cancel(&project_root, &id)?);
            Ok(())
        }
        MsgCommands::Read {
            from,
            index,
            scope,
            target,
        } => {
            let (project_root, feature) = target_project(target.project.as_deref())?;
            let target_scope = resolve_scope_with_flag(&project_root, scope)?;
            let agent = pm::messages::default_user_name();
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
            // The agent reading its own inbox is mid-turn.
            if feature.as_deref() == Some(target_scope.as_str())
                && running_agent().as_deref() == Some(agent.as_str())
                && let Some(mut window) = agent_window(server, &agent)
            {
                let unread = pm::messages::unread_count(
                    &paths::messages_dir(&project_root),
                    &target_scope,
                    &agent,
                );
                window.publish(AgentState::Busy, unread);
            }
            Ok(())
        }
        MsgCommands::List {
            from,
            scope,
            target,
        } => {
            let (project_root, _) = target_project(target.project.as_deref())?;
            let target_scope = resolve_scope_with_flag(&project_root, scope)?;
            let agent = pm::messages::default_user_name();
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
        MsgCommands::Reply {
            message,
            scope,
            target,
        } => {
            let (project_root, feature) = target_project(target.project.as_deref())?;
            let inbox_scope = match (scope, feature) {
                (Some(scope), _) => resolve_scope_with_flag(&project_root, Some(scope))?,
                (None, Some(feature)) => feature,
                (None, None) => resolve_scope(&project_root)?,
            };
            let message = read_message_body(message)?;
            let sender = pm::messages::default_user_name();
            // The reply-route rule is in `msg_reply`'s module docs.
            let home = if running_agent().is_some() {
                optional_agent_scope()?
            } else {
                None
            };
            let sent = commands::msg_reply::msg_reply(
                &paths::global_projects_dir()?,
                &project_root,
                &inbox_scope,
                &sender,
                home.as_ref().map(|(root, scope)| Home { root, scope }),
                &message,
                server,
            )?;
            report_sent(sent, server);
            push();
            Ok(())
        }
        MsgCommands::Wait {
            from,
            scope,
            target,
        } => {
            let (project_root, _) = target_project(target.project.as_deref())?;
            let target_scope = resolve_scope_with_flag(&project_root, scope)?;
            let agent = pm::messages::default_user_name();
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

/// The project a command acts on, and the scope of the agent running it
/// unless `--project` names a project other than its own.
fn target_project(project: Option<&str>) -> Result<(PathBuf, Option<String>)> {
    match project {
        Some(name) => {
            let root = project_root(&paths::global_projects_dir()?, Some(name))?;
            let own = own_scope(&root);
            Ok((root, own))
        }
        None => {
            let (root, scope) = paths::agent_scope()?;
            Ok((root, Some(scope)))
        }
    }
}

/// Read a message body from an explicit argument or stdin. Used by both
/// `pm msg send` and `pm msg reply`.
fn read_message_body(message: Option<String>) -> Result<String> {
    match message {
        Some(m) => Ok(m),
        None => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                return Err(PmError::Messaging(
                    "no message provided: pass as argument or pipe via stdin".into(),
                ));
            }
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
            let trimmed = buf.trim_end().to_string();
            if trimmed.is_empty() {
                return Err(PmError::Messaging(
                    "stdin was empty — no message to send".into(),
                ));
            }
            Ok(trimmed)
        }
    }
}

/// Parse `agent@scope` shorthand. Returns `(agent, Some(scope))` if `@` is
/// present, otherwise `(original, None)`.
fn parse_agent_at_scope(input: &str) -> (&str, Option<&str>) {
    if let Some(pos) = input.find('@') {
        let agent = &input[..pos];
        let scope = &input[pos + 1..];
        if !agent.is_empty() && !scope.is_empty() {
            return (agent, Some(scope));
        }
    }
    (input, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_at_scope_simple() {
        let (agent, scope) = parse_agent_at_scope("reviewer@main");
        assert_eq!(agent, "reviewer");
        assert_eq!(scope, Some("main"));
    }

    #[test]
    fn parse_agent_at_scope_no_at() {
        let (agent, scope) = parse_agent_at_scope("reviewer");
        assert_eq!(agent, "reviewer");
        assert_eq!(scope, None);
    }

    #[test]
    fn parse_agent_at_scope_empty_scope_ignored() {
        // "reviewer@" should not parse as shorthand
        let (agent, scope) = parse_agent_at_scope("reviewer@");
        assert_eq!(agent, "reviewer@");
        assert_eq!(scope, None);
    }

    #[test]
    fn parse_agent_at_scope_empty_agent_ignored() {
        // "@main" should not parse as shorthand
        let (agent, scope) = parse_agent_at_scope("@main");
        assert_eq!(agent, "@main");
        assert_eq!(scope, None);
    }

    #[test]
    fn parse_agent_at_scope_feature_name() {
        let (agent, scope) = parse_agent_at_scope("implementer@login-v2");
        assert_eq!(agent, "implementer");
        assert_eq!(scope, Some("login-v2"));
    }
}
