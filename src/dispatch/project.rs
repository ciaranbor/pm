//! Commands on a whole project: create, register, open, close, delete, inspect.

use std::path::PathBuf;

use pm::commands;
use pm::error::Result;
use pm::state::paths;
use pm::tmux;

use super::report::{plural, report_main_spawn};
use super::scope::project_root;
use super::window::{connect, finish_in_own_session, push};

pub(super) fn init(
    path: Option<PathBuf>,
    git: Option<String>,
    no_main: bool,
    server: Option<&str>,
) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let path = match path {
        Some(path) => path,
        None => commands::init::default_path(git.as_deref().unwrap_or_default())?,
    };
    let root = commands::init::init(&path, &projects_dir, git.as_deref(), server)?;
    if !no_main {
        report_main_spawn(&root, server);
    }
    Ok(())
}

pub(super) fn register(
    path: PathBuf,
    name: Option<String>,
    r#move: bool,
    no_main: bool,
    server: Option<&str>,
) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let root =
        commands::register::register(&path, name.as_deref(), &projects_dir, r#move, server, None)?;
    if !no_main {
        report_main_spawn(&root, server);
    }
    Ok(())
}

pub(super) fn list() -> Result<()> {
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

pub(super) fn open_all(server: Option<&str>) -> Result<()> {
    use commands::open::ProjectOpen;
    let projects_dir = paths::global_projects_dir()?;
    let mut changed = false;
    let mut outcomes =
        commands::open::open_all(&projects_dir, server, |name, outcome| match outcome {
            ProjectOpen::Opened(r) => {
                changed |= r.changed();
                for warning in &r.warnings {
                    eprintln!("warning: {name}: {warning}");
                }
                println!(
                    "{name}: restored {} session{}, respawned {} agent{}",
                    r.sessions_restored,
                    plural(r.sessions_restored),
                    r.agents_respawned,
                    plural(r.agents_respawned)
                );
            }
            ProjectOpen::RootMissing(root) => eprintln!(
                "warning: {name}: skipped, root missing at {}",
                root.display()
            ),
            ProjectOpen::Failed(e) => eprintln!("warning: {name}: {e}"),
        })?;
    commands::open::confirm_launches(
        outcomes
            .iter_mut()
            .filter_map(|(_, outcome)| match outcome {
                ProjectOpen::Opened(r) => Some(r),
                _ => None,
            }),
        server,
    );
    for (name, outcome) in &outcomes {
        if let ProjectOpen::Opened(r) = outcome {
            for failure in &r.failed_launches {
                eprintln!("error: {name}: {}", failure.message());
            }
        }
    }
    if outcomes.is_empty() {
        println!("No projects in registry");
    }
    if changed {
        push();
    }
    let current = paths::find_project_root(&std::env::current_dir()?)
        .and_then(|root| pm::state::project::ProjectConfig::load(&paths::pm_dir(&root)));
    if let Ok(config) = current {
        let main_session = tmux::session_name(&config.project.name, "main");
        if tmux::has_session(server, &main_session).unwrap_or(false) {
            connect(server, &main_session);
        }
    }
    Ok(())
}

pub(super) fn open(project: Option<String>, server: Option<&str>) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let project_root = project_root(&projects_dir, project.as_deref())?;
    let mut result = commands::open::open(&project_root, &projects_dir, server)?;
    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    commands::open::confirm_launches([&mut result], server);
    if result.changed() {
        println!(
            "Restored {} sessions. Respawned {} agents.",
            result.sessions_restored, result.agents_respawned
        );
        push();
    } else {
        println!("Project sessions opened");
    }
    for failure in &result.failed_launches {
        eprintln!("error: {}", failure.message());
    }
    connect(server, &result.main_session);
    Ok(())
}

pub(super) fn close(project: Option<String>, all: bool, server: Option<&str>) -> Result<()> {
    let own = if all {
        let (messages, own) = commands::close::close_all(server)?;
        for m in messages {
            println!("{m}");
        }
        own
    } else {
        let projects_dir = paths::global_projects_dir()?;
        let project_root = project_root(&projects_dir, project.as_deref())?;
        let closed = commands::close::close(&project_root, server)?;
        println!(
            "Closed project {} (killed {} session{})",
            closed.project,
            closed.killed,
            if closed.killed == 1 { "" } else { "s" }
        );
        closed.own
    };
    finish_in_own_session(server, own)
}

pub(super) fn delete(
    project: Option<String>,
    force: bool,
    yes: bool,
    server: Option<&str>,
) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let project_root = project_root(&projects_dir, project.as_deref())?;
    let deleted =
        commands::delete::delete(&project_root, &projects_dir, force, server, |pending| {
            for warning in pending.warnings {
                eprintln!("warning: {warning}");
            }
            if yes {
                return Ok(true);
            }
            let what = if force {
                format!(" and the checkout at {}", pending.main.display())
            } else {
                String::new()
            };
            let name = pending.project;
            match pending.features {
                0 => eprint!("Delete project '{name}'{what}? [y/N] "),
                n => eprint!("Delete project '{name}', its {n} feature(s){what}? [y/N] "),
            }
            std::io::Write::flush(&mut std::io::stderr())?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            let confirmed = answer.trim().eq_ignore_ascii_case("y");
            if !confirmed {
                eprintln!("Aborted.");
            }
            Ok(confirmed)
        })?;
    let Some(deleted) = deleted else {
        return Ok(());
    };
    for warning in &deleted.warnings {
        eprintln!("warning: {warning}");
    }
    println!("Deleted project '{}'", deleted.project);
    finish_in_own_session(server, deleted.own)
}

pub(super) fn notes(project: Option<String>) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let project_root = project_root(&projects_dir, project.as_deref())?;
    commands::notes::edit(&project_root)
}

pub(super) fn status(project: Option<String>, server: Option<&str>) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let project_root = project_root(&projects_dir, project.as_deref())?;
    let lines = commands::status::status(&project_root, &projects_dir, server)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
