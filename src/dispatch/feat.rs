use pm::commands;
use pm::error::{PmError, Result};
use pm::state::paths;

use super::report::report_failed_launches;
use super::scope::{resolve_feature_name, resolve_scope};
use super::window::{finish_in_own_session, push, running_agent};
use crate::cli::{FeatCommands, PrCommands, SummaryCommands};

pub(super) fn run(cmd: FeatCommands, server: Option<&str>) -> Result<()> {
    match cmd {
        FeatCommands::Status {
            all: true, json, ..
        } => {
            let lines =
                commands::feat_status_view::all(&paths::global_projects_dir()?, json, server)?;
            if lines.is_empty() {
                println!("No features");
            }
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        FeatCommands::List { all: true } => {
            let lines = commands::feat_list::feat_list_all(&paths::global_projects_dir()?)?;
            if lines.is_empty() {
                println!("No projects");
            }
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        cmd => in_project(cmd, server),
    }
}

fn in_project(cmd: FeatCommands, server: Option<&str>) -> Result<()> {
    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
    let projects_dir = paths::global_projects_dir()?;
    match cmd {
        FeatCommands::New {
            name,
            feature_name,
            context,
            base,
            workflow,
        } => {
            let feat_name = commands::feat_new::feat_new(&commands::feat_new::FeatNewParams {
                project_root: &project_root,
                projects_dir: &projects_dir,
                name: &name,
                name_override: feature_name.as_deref(),
                context: context.as_deref(),
                base: base.as_deref(),
                workflow: workflow.as_deref(),
                tmux_server: server,
            })?;
            println!("Created feature '{feat_name}'");
            let failed = commands::launch_check::confirm_scope(&project_root, &feat_name, server);
            push();
            report_failed_launches(&failed)
        }
        FeatCommands::Adopt {
            name,
            feature_name,
            context,
            from,
            workflow,
        } => {
            let feat_name =
                commands::feat_adopt::feat_adopt(&commands::feat_adopt::FeatAdoptParams {
                    project_root: &project_root,
                    projects_dir: &projects_dir,
                    name: &name,
                    name_override: feature_name.as_deref(),
                    context: context.as_deref(),
                    from: from.as_deref(),
                    workflow: workflow.as_deref(),
                    tmux_server: server,
                    home: None,
                })?;
            println!("Adopted feature '{feat_name}'");
            let failed = commands::launch_check::confirm_scope(&project_root, &feat_name, server);
            push();
            report_failed_launches(&failed)
        }
        // `--all` is dispatched before a project is resolved.
        FeatCommands::List { .. } => {
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
            let lines = commands::feat_info::feat_info(&project_root, &projects_dir, &name)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        FeatCommands::Delete { name, force } => {
            let name = resolve_feature_name(name, &project_root)?;
            let ended = commands::feat_delete::feat_delete(
                &project_root,
                &projects_dir,
                &name,
                force,
                server,
            )?;
            for warning in ended.warnings {
                eprintln!("warning: {warning}");
            }
            println!("Deleted feature '{name}'");
            finish_in_own_session(server, ended.own)
        }
        FeatCommands::Merge { name, keep } => {
            let name = resolve_feature_name(name, &project_root)?;
            let ended = commands::feat_merge::feat_merge(
                &project_root,
                &projects_dir,
                &name,
                keep,
                server,
            )?;
            for warning in ended.warnings {
                eprintln!("warning: {warning}");
            }
            if keep {
                println!("Merged feature '{name}'");
            } else {
                println!("Merged and deleted feature '{name}'");
            }
            finish_in_own_session(server, ended.own)
        }
        FeatCommands::Pr(pr_cmd) => match pr_cmd {
            PrCommands::Create { name, ready, body } => {
                let name = resolve_feature_name(name, &project_root)?;
                let resolved_body = body
                    .as_deref()
                    .map(commands::feat_new::resolve_context)
                    .transpose()?;
                commands::feat_pr::feat_pr(
                    &project_root,
                    &projects_dir,
                    &name,
                    ready,
                    resolved_body.as_deref(),
                )?;
                println!("PR linked for feature '{name}'");
                Ok(())
            }
            PrCommands::Edit { name, title, body } => {
                let name = resolve_feature_name(name, &project_root)?;
                let resolved_body = body
                    .as_deref()
                    .map(commands::feat_new::resolve_context)
                    .transpose()?;
                commands::feat_pr_edit::feat_pr_edit(
                    &project_root,
                    &name,
                    title.as_deref(),
                    resolved_body.as_deref(),
                )?;
                Ok(())
            }
            PrCommands::Ready { name } => {
                let name = resolve_feature_name(name, &project_root)?;
                commands::feat_pr_ready::feat_pr_ready(&project_root, &name)?;
                println!("PR marked ready for feature '{name}'");
                Ok(())
            }
        },
        FeatCommands::Status {
            status,
            name,
            reason,
            all: _,
            json,
        } => {
            use commands::feat_status::Request;
            let request =
                commands::feat_status::request(&project_root, status, name, reason.is_some())?;
            let (status, name) = match request {
                Request::Set { .. } if json => {
                    return Err(PmError::SafetyCheck(
                        "--json is for viewing; it takes no status".into(),
                    ));
                }
                Request::Set { progress, name } => (progress, name),
                Request::View { name } => {
                    let name = match name {
                        Some(name) => Some(name),
                        None if resolve_scope(&project_root)? == "main" => None,
                        None => Some(resolve_feature_name(None, &project_root)?),
                    };
                    let lines = match name {
                        Some(name) if !json => {
                            commands::feat_status_view::feature(&project_root, &name)?
                        }
                        name => commands::feat_status_view::project(
                            &project_root,
                            name.as_deref(),
                            json,
                            server,
                        )?,
                    };
                    if lines.is_empty() {
                        println!("No features");
                    }
                    for line in lines {
                        println!("{line}");
                    }
                    return Ok(());
                }
            };
            let name = resolve_feature_name(name, &project_root)?;
            commands::feat_status::feat_status(
                &project_root,
                &name,
                status,
                reason.as_deref(),
                running_agent().as_deref(),
            )?;
            println!("Feature '{name}' is {status}");
            push();
            Ok(())
        }
        FeatCommands::Summary(cmd) => match cmd {
            SummaryCommands::Path { name } => {
                let name = resolve_feature_name(name, &project_root)?;
                let path = commands::feat_summary::path(&project_root, &name)?;
                println!("{}", path.display());
                Ok(())
            }
            SummaryCommands::Show { name } => {
                let name = resolve_feature_name(name, &project_root)?;
                let summary = commands::feat_summary::show(&project_root, &name)?;
                println!("{}", summary.trim_end_matches('\n'));
                Ok(())
            }
        },
        FeatCommands::Review { pr } => {
            let feature_name = commands::feat_review::feat_review(&project_root, &pr, server)?;
            println!("Created review feature '{feature_name}'");
            push();
            Ok(())
        }
        FeatCommands::Sync { name } => {
            let name = name.or_else(|| {
                paths::detect_feature_from_cwd(&project_root, &std::env::current_dir().ok()?)
            });
            let messages = commands::feat_sync::feat_sync(&project_root, name.as_deref())?;
            for msg in messages {
                println!("{msg}");
            }
            push();
            Ok(())
        }
    }
}
