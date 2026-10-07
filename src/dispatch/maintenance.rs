//! Keeping pm and its projects healthy: doctor, upgrade, restore, migration checks.

use std::path::PathBuf;

use pm::commands;
use pm::error::Result;
use pm::harness::Probe;
use pm::state::paths;
use pm::state::project::GlobalConfig;

use super::scope::project_root;
use super::window::push;

pub(super) fn doctor(fix: bool, project: Option<String>, server: Option<&str>) -> Result<()> {
    let projects_dir = paths::global_projects_dir()?;
    let project_root = project_root(&projects_dir, project.as_deref())?;
    let lines = commands::doctor::doctor(&project_root, &projects_dir, fix, server)?.lines();
    println!("pm {}", pm::version::VERSION);
    for line in lines {
        println!("{line}");
    }
    if fix {
        push();
    }
    Ok(())
}

pub(super) fn upgrade(all: bool, dry_run: bool, server: Option<&str>) -> Result<()> {
    let lines = commands::upgrade::upgrade(all, dry_run, server)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

pub(super) fn restore(
    projects: Vec<String>,
    imports: Vec<PathBuf>,
    server: Option<&str>,
) -> Result<()> {
    let messages = commands::restore::restore(&projects, &imports, server)?;
    for msg in messages {
        println!("{msg}");
    }
    push();
    Ok(())
}

pub(super) fn migrate_check(
    projects: Vec<String>,
    verbose: bool,
    json: bool,
    server: Option<&str>,
) -> Result<()> {
    let (config_dir, home, projects_dir) = commands::migrate_check::resolve()?;
    let report = commands::migrate_check::check(&commands::migrate_check::CheckParams {
        projects_dir: &projects_dir,
        config_dir: &config_dir,
        home: &home,
        projects: &projects,
        tmux_server: server,
        probe: Probe::Fresh,
        global_harness: &GlobalConfig::load_or_default().harness,
    })?;
    if json {
        println!("{:#}", report.json());
    } else {
        let style = commands::migrate_check::Style::detect();
        for line in report.lines(style, verbose) {
            println!("{line}");
        }
    }
    if report.blockers() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

pub(super) fn self_update(force: bool) -> Result<()> {
    let lines = commands::self_update::self_update(force)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
