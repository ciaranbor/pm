use pm::commands;
use pm::commands::attention::AgentState;
use pm::error::{PmError, Result};
use pm::state::paths;

use super::report::report_sent;
use super::scope::resolve_scope_with_flag;
use super::window::{agent_window, push, running_agent};
use crate::cli::MsgCommands;

pub(super) fn run(cmd: MsgCommands, server: Option<&str>) -> Result<()> {
    let (project_root, feature) = paths::agent_scope()?;
    match cmd {
        MsgCommands::Send {
            agent,
            message,
            as_agent,
            scope,
            upstream,
            project: target_project,
        } => {
            let message = read_message_body(message)?;
            let sender = as_agent.unwrap_or_else(pm::messages::default_user_name);

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

            if let Some(ref proj_name) = target_project {
                let target_scope = effective_scope.as_deref().unwrap_or("main");
                let pm_dir = paths::pm_dir(&project_root);
                let sender_project_config = pm::state::project::ProjectConfig::load(&pm_dir)?;
                let sender_project_name = &sender_project_config.project.name;
                let line = commands::agent_send::agent_send_cross_project(
                    &commands::agent_send::CrossProjectSendParams {
                        target_project_name: proj_name,
                        sender_scope: &feature,
                        sender_project: sender_project_name,
                        target_scope,
                        recipient,
                        sender: &sender,
                        body: &message,
                    },
                )?;
                println!("{line}");
            } else {
                let target_scope = if upstream {
                    let main_branch = pm::state::project::ProjectEntry::main_branch(
                        &project_root,
                        &paths::global_projects_dir()?,
                    )?;
                    Some(commands::agent_send::resolve_upstream(
                        &project_root,
                        &main_branch,
                        &feature,
                    )?)
                } else {
                    effective_scope
                };
                let sent = commands::agent_send::agent_send(
                    &project_root,
                    &feature,
                    target_scope.as_deref(),
                    recipient,
                    &sender,
                    &message,
                    server,
                )?;
                report_sent(&project_root, sent, server);
            }
            push();
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
            // The agent reading its own inbox is mid-turn.
            if target_scope == feature
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
        MsgCommands::Reply { message, as_agent } => {
            let message = read_message_body(message)?;
            let sender = as_agent.unwrap_or_else(pm::messages::default_user_name);
            let sent =
                commands::msg_reply::msg_reply(&project_root, &feature, &sender, &message, server)?;
            report_sent(&project_root, sent, server);
            push();
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
