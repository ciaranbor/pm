//! pm's own calls to opencode: every one a `--standalone` command line run
//! with stdin closed and the calling agent's environment scrubbed.

use std::process::{Command, Stdio};

use serde_json::Value;

use super::{
    CONFIG_CONTENT_ENV, CONFIG_ENV, DEFAULT_BINARY, PROMPT_ENV, SESSION_ENV, TRIP_ENV, argv,
};
use crate::bounded;
use crate::error::{PmError, Result};
use crate::state::paths;
use crate::state::project::OpenCodeConfig;

/// The limit for a call that answers from opencode's own store.
pub(super) const CALL: std::time::Duration = crate::harness::CALL_LIMIT;

/// The limit for a call that writes or reads a whole transcript.
pub(super) const TRANSFER: std::time::Duration = std::time::Duration::from_secs(300);

/// Removed from pm's own `opencode api` calls. pm often runs inside an
/// agent: neither that agent's identity nor its opencode config may reach a
/// server started on another agent's behalf.
const SCRUBBED_ENV: &[&str] = &[
    "PM_AGENT_NAME",
    paths::AGENT_WORKTREE_ENV,
    SESSION_ENV,
    PROMPT_ENV,
    TRIP_ENV,
    CONFIG_ENV,
    CONFIG_CONTENT_ENV,
];

/// A non-interactive `opencode <subcommand> <args>`, with stdin closed
/// because `api` otherwise waits on it.
pub(super) fn command(cfg: &OpenCodeConfig, subcommand: &[&str], args: &[&str]) -> Command {
    let argv = argv(cfg, subcommand, args);
    let mut command = detached(&argv[0]);
    command.args(&argv[1..]);
    command
}

/// `program` as pm runs opencode itself: stdin closed, and nothing of the
/// calling agent's identity or opencode config in its environment.
pub(super) fn detached(program: &str) -> Command {
    let mut command = Command::new(executable(program));
    command.stdin(Stdio::null());
    for key in SCRUBBED_ENV {
        command.env_remove(key);
    }
    command
}

/// `program` to execute. A lib test runs opencode only through a fake it
/// configures, never the developer's own and its session store.
pub(super) fn executable(program: &str) -> &str {
    if cfg!(test) && program == DEFAULT_BINARY {
        "pm-test-opencode-not-configured"
    } else {
        program
    }
}

pub(super) fn api(cfg: &OpenCodeConfig, args: &[&str]) -> Result<Value> {
    run_api(command(cfg, &["api"], args), args)
}

/// Run an `opencode api` command line for the operation `args` names and
/// read its answer.
pub(super) fn run_api(command: Command, args: &[&str]) -> Result<Value> {
    let operation = args.first().copied().unwrap_or_default();
    try_api(command, args)?.map_err(|refusal| {
        PmError::Agent(format!("opencode {operation} failed: {}", refusal.message))
    })
}

/// What opencode answered a call it refused with: the `_tag` of its error
/// object, when it printed one, and the message to show.
#[derive(Debug)]
pub(super) struct Refusal {
    pub(super) tag: Option<String>,
    pub(super) message: String,
}

/// [`run_api`], with a call opencode refused handed back instead of
/// reported, for a caller that acts on the refusal.
pub(super) fn try_api(
    mut command: Command,
    args: &[&str],
) -> Result<std::result::Result<Value, Refusal>> {
    let operation = args.first().copied().unwrap_or_default();
    let out = bounded::output(&mut command, &format!("opencode {operation}"), CALL)?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Ok(Err(refusal(&stdout, &String::from_utf8_lossy(&out.stderr))));
    }
    // A call that succeeds with nothing to return prints nothing.
    if stdout.trim().is_empty() {
        return Ok(Ok(Value::Null));
    }
    serde_json::from_str(stdout.trim()).map(Ok).map_err(|e| {
        PmError::Agent(format!(
            "opencode {operation} returned unreadable output ({e}): {}",
            stdout.trim()
        ))
    })
}

/// The error object opencode prints on stdout, else whatever it printed.
pub(super) fn refusal(stdout: &str, stderr: &str) -> Refusal {
    stdout
        .lines()
        .find_map(|line| {
            let error = serde_json::from_str::<Value>(line).ok()?;
            Some(Refusal {
                message: error.get("message")?.as_str()?.to_string(),
                tag: error
                    .get("_tag")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .unwrap_or_else(|| Refusal {
            tag: None,
            message: format!("{} {}", stdout.trim(), stderr.trim())
                .trim()
                .to_string(),
        })
}
