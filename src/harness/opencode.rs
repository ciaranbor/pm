//! opencode v2: `opencode --standalone --auto --session <id>`, with the
//! session created by pm before the TUI starts.
//!
//! **Every invocation carries `--standalone`** ([`argv`] is the only place a
//! command line is built). Without it any server-needing command starts a
//! shared `opencode serve --service` per `$HOME`, whose plugins see the
//! environment of whichever client started it — a second agent is then
//! driven under the first one's `PM_AGENT_NAME`. The one exception is a
//! session move, which needs a server that outlives the request and gets one
//! of its own ([`sessions`]).
//!
//! opencode has no Stop hook. The never-idle loop is the bundled
//! `pm-never-idle` plugin (`bundled/plugins/opencode/pm-never-idle/`), installed once
//! per machine under the user-level plugins dir: on each turn end it blocks
//! in `pm harness hooks stop` and prompts the session with the answer. It
//! also appends pm's composed prompt to the system prompt of every model
//! request, which is how the baseline and notice boards arrive.
//! The plugin is loaded at launch: a running session keeps the one it
//! started with.
//!
//! opencode also unloads a directory's plugins after 60 minutes without a
//! session event there (hardcoded, with no keep-alive a plugin can hold),
//! and loads them again only when a request needs that directory. So an
//! unload between turns leaves the plugin's wait running: the next load
//! retires it, and with none its answer prompts the session through the
//! unloaded plugin's handle, which loads the plugin again. That handle
//! outliving its unload is opencode 2.0.24's behaviour, not a documented
//! contract; if the prompt fails, or no load follows it, the plugin records
//! why for `pm doctor`.
//!
//! opencode has no UserPromptSubmit hook either. The plugin watches
//! `session.inbox.enqueued` for `user` items instead and runs `pm harness
//! hooks user-prompt` for each. Its own prompts are `user` items too, so it
//! marks them with `metadata`, which `session.prompt` copies onto the item.
//!
//! A turn end is the only event the plugin can arm on, and neither a new nor
//! a resumed session emits one, so [`pre_launch`] settles the session id
//! before the TUI starts — created, resumed, or forked through `opencode
//! api` — and hands it to the plugin as `PM_OPENCODE_SESSION`, which arms at
//! setup. No first model turn is needed, so the spawn's sentinel prompt is
//! dropped.
//!
//! opencode does not read `.agents/agents/`: definitions are projected to
//! `<config dir>/agents/` and selected by name at `session.create`. An
//! unknown name is **silent** — the session runs the built-in prompt — and
//! no opencode command lists config-defined agents, so a spawn refuses a
//! definition whose projected file it cannot find. Skills need no
//! projection (`.agents/skills` is read directly), and there is no
//! directory or plugin trust gate.
//!
//! Per-agent settings reach opencode through a config file written for
//! every spawn and named by `OPENCODE_CONFIG`. It is always written, and
//! `OPENCODE_CONFIG_CONTENT` always removed, so an agent never runs on the
//! config of whoever spawned it.
//!
//! The `[agents.models]` row is `<provider>/<model>[#variant]`, and an
//! agent without one is refused: opencode does not reject a model it cannot
//! resolve — not in config, not at `session.create`, not at
//! `session.switchModel` — and a session whose model comes from config runs
//! on opencode's default model, which with no provider configured is a
//! hosted one. So the row is **pinned on the session** (at creation, and
//! again on every resume and fork, which is what makes an edited row
//! apply), and the config limits the agent (`enabled_providers`) to the
//! row's provider and the ones pm config defines ([`providers`]). A pinned
//! model opencode cannot resolve fails the turn with `Model unavailable`
//! before any request is made; the plugin then stops the loop and reports
//! the turn's error.
//!
//! The plugin reports to `pm doctor` through files in the agent's runtime
//! dir, which a spawn clears: why its loop stopped, the error of a failed
//! turn until a later one succeeds, and that its setup ran at all. Without
//! that last one a TUI that never loads the plugin is indistinguishable
//! from an agent waiting for mail, since the session id is recorded before
//! the TUI starts.
//!
//! A resumed session opencode no longer holds is refused by the pin
//! (`SessionNotFoundError`), and the spawn starts a fresh one instead.
//!
//! The `[agents.permissions]` row is opencode's own rule list, a JSON array
//! such as `[{"action":"edit","resource":"*","effect":"deny"}]`; the last
//! matching rule wins. `--auto` approves whatever no rule denies and is the
//! default, because an approval prompt in an unwatched window stalls the
//! agent. With `[harness.opencode] auto = false` pm puts allow rules for its
//! own state dirs ahead of the row, which can still override them.

mod api;
pub(super) mod chat;
mod config;
pub(super) mod dialog;
pub(super) mod input;
mod messages;
mod provider_check;
mod providers;
mod reach;
pub(super) mod sessions;
mod status_files;
mod version;
pub(super) mod waiting;

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::{LaunchContext, PreLaunch, Projection, ProjectionScope, SpawnSpec, Wake};
use crate::state::project::OpenCodeConfig;
use crate::state::workflow::VANILLA_AGENT;
use crate::tmux;
use config::{ModelRef, render_config};
use sessions::{create_session, fork_session, pin_model};
use status_files::{clear_status_files, spawn_file};

pub(super) use config::{row_issues, row_notes};
pub(super) use status_files::{loaded_file, trip_file, turn_error_file};
pub(super) use version::{
    installed_version, min_version_string, unusable_reason, version_supported,
};

pub(super) const CONFIG_DIR: &str = ".opencode";
pub(super) const PROJECTED_DIRS: &[&str] = &["agents"];

const DEFAULT_BINARY: &str = "opencode";

/// opencode keeps its provider credentials in its own files.
pub(super) const READS_KEYCHAIN: bool = false;
const PLUGIN_DIR: &str = "plugins/pm-never-idle";

/// The plugin as installed, relative to [`PLUGIN_DIR`]. `index.ts` is what
/// opencode loads.
const PLUGIN_FILES: &[(&str, &str)] = &[
    (
        "index.ts",
        include_str!("../../bundled/plugins/opencode/pm-never-idle/index.ts"),
    ),
    (
        "loop.ts",
        include_str!("../../bundled/plugins/opencode/pm-never-idle/loop.ts"),
    ),
    (
        "pm.ts",
        include_str!("../../bundled/plugins/opencode/pm-never-idle/pm.ts"),
    ),
];

const SESSION_ENV: &str = "PM_OPENCODE_SESSION";
const PROMPT_ENV: &str = "PM_APPEND_PROMPT_FILE";
const TRIP_ENV: &str = "PM_OPENCODE_TRIP_FILE";
const CONFIG_ENV: &str = "OPENCODE_CONFIG";
const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// `$XDG_CONFIG_HOME/opencode`, else `<home>/.config/opencode`. Tests always
/// get the latter so a developer's own environment never leaks into a test
/// home.
pub(super) fn global_dir(home: &Path) -> PathBuf {
    #[cfg(not(test))]
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("opencode");
    }
    home.join(".config").join("opencode")
}

pub(super) fn project_assets(
    canonical_root: &Path,
    target_root: &Path,
    scope: &ProjectionScope<'_>,
    dry_run: bool,
) -> Result<Projection> {
    super::project_by_copy(canonical_root, target_root, PROJECTED_DIRS, scope, dry_run)
}

/// The plugin's files under `home` with their bundled content.
pub(super) fn plugin_files(home: &Path) -> Vec<(PathBuf, &'static str)> {
    let dir = global_dir(home).join(PLUGIN_DIR);
    PLUGIN_FILES
        .iter()
        .map(|(name, content)| (dir.join(name), *content))
        .collect()
}

pub(super) fn binary(cfg: &OpenCodeConfig) -> &str {
    cfg.binary.as_deref().unwrap_or(DEFAULT_BINARY)
}

/// An opencode command line: the binary, the subcommand's words if any,
/// then `--standalone` ahead of everything else. opencode takes the flag
/// only after the last subcommand word (`session export --standalone`).
fn argv(cfg: &OpenCodeConfig, subcommand: &[&str], rest: &[&str]) -> Vec<String> {
    let mut out = vec![binary(cfg).to_string()];
    out.extend(subcommand.iter().map(|s| s.to_string()));
    out.push("--standalone".to_string());
    out.extend(rest.iter().map(|s| s.to_string()));
    out
}

/// `env -u <removed>… <KEY=value>… opencode …`, every word quoted: the
/// session id comes from opencode's answer or the registry, the rest from
/// config.
pub(super) fn build_cmd(_spec: &SpawnSpec<'_>, cfg: &OpenCodeConfig, pre: &PreLaunch) -> String {
    let mut rest = Vec::new();
    if cfg.auto != Some(false) {
        rest.push("--auto");
    }
    if let Some(id) = &pre.session_id {
        rest.extend(["--session", id]);
    }
    let mut words = vec!["env".to_string()];
    for key in &pre.env_remove {
        words.extend(["-u".to_string(), key.clone()]);
    }
    words.extend(pre.env.iter().map(|(key, value)| format!("{key}={value}")));
    words.extend(argv(cfg, &[], &rest));
    words
        .iter()
        .map(|word| tmux::shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Settle the session the TUI will open and write the files its environment
/// names.
pub(super) fn pre_launch(
    ctx: &LaunchContext<'_>,
    spec: &SpawnSpec<'_>,
    cfg: &OpenCodeConfig,
) -> Result<PreLaunch> {
    let Some(row) = spec.model else {
        return Err(PmError::Agent(format!(
            "opencode agent '{}' has no [agents.models] row; without one opencode picks the \
             model itself, a hosted one unless its own config says otherwise. Set \
             `[agents.models] {} = \"<provider>/<model>\"`",
            ctx.agent,
            spec.definition.unwrap_or(VANILLA_AGENT)
        )));
    };
    let model = ModelRef::parse(row)?;
    if let Some(def) = spec.definition {
        unprojected_definition(ctx, def, &crate::state::paths::home_dir()?)?;
    }
    let config = render_config(spec, cfg, row, &model)?;

    let mut notes = Vec::new();
    let session_id = match spec.resume_session {
        Some(source) if spec.fork_session => {
            let id = fork_session(cfg, source)?;
            if !pin_model(cfg, &id, &model)? {
                return Err(PmError::Agent(format!(
                    "opencode forked session {source} as {id}, then did not find {id} to pin \
                     the model on"
                )));
            }
            id
        }
        Some(source) => {
            if pin_model(cfg, source, &model)? {
                source.to_string()
            } else {
                notes.push(format!(
                    "opencode no longer has session {source}; previous session not resumed"
                ));
                create_session(cfg, ctx, spec.definition, &model)?
            }
        }
        None => create_session(cfg, ctx, spec.definition, &model)?,
    };

    clear_status_files(ctx.project_root, ctx.feature, ctx.agent)?;
    let mut env = vec![(SESSION_ENV.to_string(), session_id.clone())];
    if let Some(file) = spec.append_prompt_file {
        env.push((PROMPT_ENV.to_string(), file.to_string()));
    }
    env.push((
        TRIP_ENV.to_string(),
        trip_file(ctx.project_root, ctx.feature, ctx.agent)?
            .to_string_lossy()
            .into_owned(),
    ));
    let path = spawn_file(ctx.project_root, ctx.feature, ctx.agent, "json")?;
    write_atomic(&path, format!("{config:#}\n").as_bytes())?;
    notes.extend(providers::undeclared_model_note(&cfg.providers, &model));
    // `pm doctor` covers the providers this agent's row does not name.
    let own = cfg.providers.get_key_value(model.provider);
    notes.extend(providers::split_model_id_notes(own));
    notes.extend(providers::unset_key_notes(
        own,
        providers::set_in_environment,
    ));
    env.push((CONFIG_ENV.to_string(), path.to_string_lossy().into_owned()));
    Ok(PreLaunch {
        session_id: Some(session_id),
        env,
        env_remove: vec![CONFIG_CONTENT_ENV.to_string()],
        notes,
    })
}

/// Refuse `definition` when opencode, started in the agent's worktree, would
/// not find it. `pm upgrade` projects into main and the global dir only; a
/// feature worktree gets main's copy from `pm harness pull`.
fn unprojected_definition(ctx: &LaunchContext<'_>, definition: &str, home: &Path) -> Result<()> {
    let opencode = super::Harness::OpenCode;
    if opencode.definition_projected(ctx.worktree, home, definition) {
        return Ok(());
    }
    let main = crate::state::paths::main_worktree(ctx.project_root);
    let pull = format!("`pm harness pull {}`", ctx.feature);
    let fix = if ctx.worktree == main {
        "run `pm upgrade`".to_string()
    } else if opencode.definition_projected(&main, home, definition) {
        format!("run {pull}")
    } else {
        format!("run `pm upgrade`, then {pull}")
    };
    Err(PmError::Agent(format!(
        "definition '{definition}' is not projected for opencode, which would run agent '{}' \
         without its role; {fix}",
        ctx.agent
    )))
}

/// What `pm doctor` reports about `[harness.opencode]` for agents started
/// in `worktree`.
pub(super) fn config_issues(
    cfg: &OpenCodeConfig,
    worktree: &Path,
    rows: &[String],
) -> Vec<super::ConfigIssue> {
    provider_check::config_issues(cfg, worktree, rows)
}

/// The manual step that gives a new machine's opencode its credentials:
/// the variables pm config names for its providers' keys, and opencode's
/// own login for any provider pm config does not define.
pub(super) fn credentials_step(cfg: &OpenCodeConfig) -> String {
    let variables = providers::key_variables(&cfg.providers);
    let own = "`opencode auth login` for providers pm config does not define";
    if variables.is_empty() {
        own.to_string()
    } else {
        format!(
            "set {} in the environment agents start in, and {own}",
            variables.join(", ")
        )
    }
}

pub(super) const EXPORT_TAG: &str = "opencode";
pub(super) const WAKE: Wake = Wake::Block;

pub(super) fn prompt_mechanism() -> String {
    format!(
        "the pm-never-idle plugin's context hook (needs opencode >= {})",
        min_version_string()
    )
}

#[cfg(test)]
pub(super) mod test_support {
    use std::path::Path;

    use crate::state::project::OpenCodeConfig;

    /// A config whose binary is a fake opencode answering `answer` and exiting `exit`.
    pub fn fake_opencode(dir: &Path, answer: &str, exit: i32) -> OpenCodeConfig {
        OpenCodeConfig {
            binary: Some(crate::testing::fake_opencode(dir, answer, exit)),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests;
