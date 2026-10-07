//! Printing the outcome of commands that act on several agents.

use std::path::Path;

use pm::commands;
use pm::error::{PmError, Result};

use super::window::push;

/// Spawn a new project's `main` agent. The project is set up by now, so a
/// failure is reported with the command that retries it, not returned.
pub(super) fn report_main_spawn(project_root: &Path, server: Option<&str>) {
    match commands::init::spawn_main(project_root, server) {
        Ok(msg) => println!("{msg}"),
        Err(e) => eprintln!(
            "warning: could not spawn the main agent: {e}; run `pm agent spawn main` in the \
             main session"
        ),
    }
}

/// Print the per-agent results of a multi-name agent op (stop, delete,
/// restart), aggregating errors. Continues on error: each result is
/// printed individually and a single `PmError::Agent` is returned only
/// if any failed, so partial successes are still observable on stdout.
pub(super) fn report_agent_op_results(results: Vec<Result<String>>, op_label: &str) -> Result<()> {
    let mut had_error = false;
    for result in results {
        match result {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("error: {e}");
                had_error = true;
            }
        }
    }
    if had_error {
        Err(PmError::Agent(format!("some agents failed to {op_label}")))
    } else {
        Ok(())
    }
}

/// Run `pm agent restart --all` over `scopes`: a line per agent and a
/// summary, then the caller's old pane is killed. Skips aren't failures.
pub(super) fn report_restart_all(
    scopes: &[commands::agent_restart_all::Scope],
    unread: Vec<commands::agent_restart_all::Report>,
    select: commands::agent_restart_all::Select,
    force: bool,
    server: Option<&str>,
) -> Result<()> {
    use commands::agent_restart_all::Outcome;
    let mut done = commands::agent_restart_all::restart_all(scopes, select, force, server)?;
    done.confirm_launches(server);
    let mut sweep = done.sweep();
    sweep.reports.splice(0..0, unread);
    for report in &sweep.reports {
        match report.outcome {
            Outcome::Failed(_) | Outcome::NotUp(_) => eprintln!("{report}"),
            _ => println!("{report}"),
        }
    }
    println!("{}", sweep.summary());
    let failed = sweep.count(|o| matches!(o, Outcome::Failed(_) | Outcome::NotUp(_)));
    let not_up = sweep.count(|o| matches!(o, Outcome::NotUp(_)));
    std::io::Write::flush(&mut std::io::stdout())?;
    done.finish(server);
    push();
    if failed > 0 {
        let failure = commands::agent_restart::not_up_failure(failed, not_up)
            .unwrap_or("some agents failed to restart");
        return Err(PmError::Agent(failure.to_string()));
    }
    Ok(())
}

pub(super) fn restart_select(stale: bool) -> commands::agent_restart_all::Select {
    if stale {
        commands::agent_restart_all::Select::Stale
    } else {
        commands::agent_restart_all::Select::All
    }
}

/// Print what a send did, reporting a respawned recipient only once its
/// harness has come up. The message is queued either way, so a failed
/// launch is a warning.
pub(super) fn report_sent(
    project_root: &Path,
    sent: commands::agent_send::Sent,
    server: Option<&str>,
) {
    println!("{}", sent.status);
    if let Some(heal) = sent.heal {
        let agents = [heal.agent];
        match commands::launch_check::confirm(project_root, &heal.scope, &agents, server).first() {
            None => println!("{}", heal.report),
            Some(failure) => eprintln!("warning: {}", failure.message()),
        }
    }
}

/// Print each launch that exited or did not come up; an error saying how
/// many, if any did.
pub(super) fn report_failed_launches(
    failed: &[commands::launch_check::FailedLaunch],
) -> Result<()> {
    for failure in failed {
        eprintln!("error: {}", failure.message());
    }
    let not_up = failed.iter().filter(|f| f.not_up()).count();
    let counts = [
        (failed.len() - not_up, "exited at launch"),
        (not_up, "did not come up"),
    ];
    let said: Vec<String> = counts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} agent{} {what}", plural(*n)))
        .collect();
    if said.is_empty() {
        Ok(())
    } else {
        Err(PmError::Agent(said.join(", ")))
    }
}

pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
