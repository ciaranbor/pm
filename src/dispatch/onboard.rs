//! `pm onboard`: the terminal side of [`commands::onboard`] — the harness
//! report, the one question, and leaving the user in home's session.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use pm::commands;
use pm::commands::onboard::{self, Readiness};
use pm::commands::skills::GlobalStore;
use pm::error::{PmError, Result};
use pm::harness::Harness;
use pm::state::paths;
use pm::state::project::GlobalConfig;

use super::window::{connect, push};

pub(super) fn run(harness: Option<Harness>, server: Option<&str>) -> Result<()> {
    let dirs = paths::global_dirs()?;
    let projects_dir = paths::global_projects_dir()?;
    let root = onboard::home_root(&dirs);
    let root = match onboard::find(&projects_dir, &root)? {
        Some(root) => {
            if harness.is_some() {
                eprintln!(
                    "warning: home exists, so --harness is ignored; `pm config set --global \
                     agents.harness.{} <harness>` changes it",
                    onboard::SUPERVISOR
                );
            }
            println!("Opening home at {}", display(&root));
            root
        }
        None => {
            create(&root, &projects_dir, &dirs.config, harness, server)?;
            root
        }
    };
    onboard::write_shadow(&root)?;
    onboard::install_skill(&root)?;
    onboard::mark(&dirs.config)?;

    let mut opened = commands::open::open(&root, &projects_dir, server)?;
    for warning in &opened.warnings {
        eprintln!("warning: {warning}");
    }
    commands::open::confirm_launches([&mut opened], server);
    for failure in &opened.failed_launches {
        eprintln!("error: {}", failure.message());
    }
    if let Some(spawned) = onboard::spawn_supervisor(&root, server)? {
        println!("{spawned}");
    }
    push();
    if std::env::var_os("TMUX").is_some() || std::io::stdin().is_terminal() {
        connect(server, &opened.main_session);
    } else {
        println!(
            "Home is up; `pm open --project {}` attaches to it",
            onboard::HOME_NAME
        );
    }
    Ok(())
}

/// Choose the supervisor's harness and create home at `root`.
fn create(
    root: &Path,
    projects_dir: &Path,
    config_dir: &Path,
    harness: Option<Harness>,
    server: Option<&str>,
) -> Result<()> {
    let store = GlobalStore::resolve()?;
    onboard::install(&store)?;

    let global = GlobalConfig::load(config_dir)?;
    let statuses = onboard::readiness(&global, root, &store.home)?;
    println!("Harnesses for the supervisor:");
    for status in &statuses {
        match status.problems.as_slice() {
            [] => println!("  {}: ready", status.harness),
            problems => {
                println!("  {}: not ready", status.harness);
                for problem in problems {
                    println!("    {problem}");
                }
            }
        }
    }
    let configured = global
        .agents
        .harness
        .get(onboard::SUPERVISOR)
        .and_then(|h| h.parse().ok());
    let chosen = match harness {
        Some(h) => h,
        None => choose(&statuses, onboard::default_choice(&statuses, configured))?,
    };
    if let Some(status) = statuses.iter().find(|s| s.harness == chosen)
        && !status.problems.is_empty()
    {
        eprintln!("warning: {chosen} is not ready; the supervisor may not start until it is");
    }
    for line in onboard::set_harness(config_dir, chosen)? {
        println!("{line}");
    }
    onboard::create(root, projects_dir, &store, server)?;
    println!("Created home at {}", display(root));
    Ok(())
}

/// Ask which ready harness to use, `default` on an empty answer or with no
/// terminal to ask at.
fn choose(statuses: &[Readiness], default: Option<Harness>) -> Result<Harness> {
    let Some(default) = default else {
        return Err(PmError::Onboard(
            "no harness is ready for the supervisor; fix one of the above and run `pm \
             onboard` again, or pass --harness to use one anyway"
                .to_string(),
        ));
    };
    let ready: Vec<Harness> = statuses
        .iter()
        .filter(|s| s.problems.is_empty())
        .map(|s| s.harness)
        .collect();
    if ready.len() < 2 || !std::io::stdin().is_terminal() {
        println!("Using {default}");
        return Ok(default);
    }
    let names: Vec<String> = ready.iter().map(Harness::to_string).collect();
    loop {
        eprint!(
            "Which harness should the supervisor use? ({}) [{default}] ",
            names.join(", ")
        );
        std::io::stderr().flush()?;
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer)? == 0 {
            return Ok(default);
        }
        let answer = answer.trim();
        if answer.is_empty() {
            return Ok(default);
        }
        match answer.parse::<Harness>() {
            Ok(h) if ready.contains(&h) => return Ok(h),
            _ => eprintln!("Answer one of: {}", names.join(", ")),
        }
    }
}

fn display(root: &Path) -> String {
    PathBuf::from(pm::path_utils::to_portable(root))
        .display()
        .to_string()
}
