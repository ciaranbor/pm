//! opencode v2: `opencode --standalone --auto --session <id>`, with the
//! session created by pm before the TUI starts.
//!
//! **Every invocation carries `--standalone`** ([`argv`] is the only place a
//! command line is built). Without it any server-needing command starts a
//! shared `opencode serve --service` per `$HOME`, whose plugins see the
//! environment of whichever client started it — a second agent is then
//! driven under the first one's `PM_AGENT_NAME`.
//!
//! opencode has no Stop hook. The never-idle loop is the bundled
//! `pm-never-idle` plugin (`plugins/opencode/pm-never-idle/`), installed once
//! per machine under the user-level plugins dir: on each turn end it blocks
//! in `pm harness hooks stop` and prompts the session with the answer. It
//! also appends pm's composed prompt to the system prompt of every model
//! request, which is how the baseline and notice boards arrive.
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
//! no opencode command lists config-defined agents, so `pm doctor` checks the
//! projected file instead. Skills need no projection (`.agents/skills` is
//! read directly), and there is no directory or plugin trust gate.
//!
//! Per-agent settings reach opencode through a config file written for
//! every spawn and named by `OPENCODE_CONFIG`. It is always written, and
//! `OPENCODE_CONFIG_CONTENT` always removed, so an agent never runs on the
//! config of whoever spawned it.
//!
//! The `[agents.models]` row is `<provider>/<model>[#variant]`. opencode
//! does not reject a model it cannot resolve — not in config, not at
//! `session.create`, not at `session.switchModel` — and a session whose
//! model comes from config then runs on opencode's default model, which with
//! no provider configured is a hosted one. So the row is also **pinned on
//! the session** (at creation, and again on every resume and fork, which is
//! what makes an edited row apply), and the config limits the agent to the
//! row's provider (`enabled_providers`). A pinned model opencode cannot
//! resolve fails the turn with `Model unavailable` before any request is
//! made; the plugin then stops the loop and reports it. opencode cannot
//! unpin a session, so once the row is removed a pinned session is not
//! resumed: the agent starts a fresh one, and a fork is refused.
//!
//! The `[agents.permissions]` row is opencode's own rule list, a JSON array
//! such as `[{"action":"edit","resource":"*","effect":"deny"}]`; the last
//! matching rule wins. `--auto` approves whatever no rule denies and is the
//! default, because an approval prompt in an unwatched window stalls the
//! agent. With `[harness.opencode] auto = false` pm puts allow rules for its
//! own state dirs ahead of the row, which can still override them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::error::{PmError, Result};
use crate::fs_utils::write_atomic;
use crate::harness::{LaunchContext, PreLaunch, Projection, SpawnSpec};
use crate::state::project::OpenCodeConfig;
use crate::tmux;

pub(super) const CONFIG_DIR: &str = ".opencode";
pub(super) const PROJECTED_DIRS: &[&str] = &["agents"];

/// Earliest release with the events and plugin API the loop relies on.
pub(super) const MIN_VERSION: (u32, u32, u32) = (2, 0, 18);

const DEFAULT_BINARY: &str = "opencode";
const PLUGIN_DIR: &str = "plugins/pm-never-idle";

/// The plugin as installed, relative to [`PLUGIN_DIR`]. `index.ts` is what
/// opencode loads.
const PLUGIN_FILES: &[(&str, &str)] = &[
    (
        "index.ts",
        include_str!("../../plugins/opencode/pm-never-idle/index.ts"),
    ),
    (
        "loop.ts",
        include_str!("../../plugins/opencode/pm-never-idle/loop.ts"),
    ),
    (
        "pm.ts",
        include_str!("../../plugins/opencode/pm-never-idle/pm.ts"),
    ),
];

const SESSION_ENV: &str = "PM_OPENCODE_SESSION";
const PROMPT_ENV: &str = "PM_APPEND_PROMPT_FILE";
const TRIP_ENV: &str = "PM_OPENCODE_TRIP_FILE";
const CONFIG_ENV: &str = "OPENCODE_CONFIG";
const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// Removed from pm's own `opencode api` calls. pm often runs inside an
/// agent: neither that agent's identity nor its opencode config may reach a
/// server started on another agent's behalf.
const SCRUBBED_ENV: &[&str] = &[
    "PM_AGENT_NAME",
    SESSION_ENV,
    PROMPT_ENV,
    TRIP_ENV,
    CONFIG_ENV,
    CONFIG_CONTENT_ENV,
];

/// An `[agents.models]` row, as opencode's session API takes it.
#[derive(Debug, PartialEq, Eq)]
struct ModelRef<'a> {
    provider: &'a str,
    id: &'a str,
    variant: Option<&'a str>,
}

impl<'a> ModelRef<'a> {
    /// `<provider>/<model>[#variant]`. The model id may itself hold `/`.
    fn parse(row: &'a str) -> Result<Self> {
        let (model, variant) = match row.split_once('#') {
            Some((model, variant)) => (model, Some(variant)),
            None => (row, None),
        };
        match model.split_once('/') {
            Some((provider, id))
                if !provider.is_empty() && !id.is_empty() && variant != Some("") =>
            {
                Ok(Self {
                    provider,
                    id,
                    variant,
                })
            }
            _ => Err(PmError::Agent(format!(
                "[agents.models] row for an opencode agent must be \
                 `<provider>/<model>[#variant]`; got: {row}"
            ))),
        }
    }

    fn to_json(&self) -> Value {
        let mut out = json!({"providerID": self.provider, "id": self.id});
        if let Some(variant) = self.variant {
            out["variant"] = json!(variant);
        }
        out
    }
}

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
    dry_run: bool,
) -> Result<Projection> {
    super::project_by_copy(canonical_root, target_root, PROJECTED_DIRS, dry_run)
}

/// The plugin's files under `home` with their bundled content.
pub(super) fn plugin_files(home: &Path) -> Vec<(PathBuf, &'static str)> {
    let dir = global_dir(home).join(PLUGIN_DIR);
    PLUGIN_FILES
        .iter()
        .map(|(name, content)| (dir.join(name), *content))
        .collect()
}

fn binary(cfg: &OpenCodeConfig) -> &str {
    cfg.binary.as_deref().unwrap_or(DEFAULT_BINARY)
}

/// An opencode command line: the binary, the subcommand if any, then
/// `--standalone` ahead of everything else.
fn argv(cfg: &OpenCodeConfig, subcommand: Option<&str>, rest: &[&str]) -> Vec<String> {
    let mut out = vec![binary(cfg).to_string()];
    out.extend(subcommand.map(str::to_string));
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
    words.extend(argv(cfg, None, &rest));
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
    let model = spec.model.map(ModelRef::parse).transpose()?;
    let config = render_config(spec, cfg, model.as_ref())?;

    let mut notes = Vec::new();
    let session_id = match (spec.resume_session, &model) {
        (Some(source), Some(model)) => {
            let id = if spec.fork_session {
                fork_session(cfg, source)?
            } else {
                source.to_string()
            };
            pin_model(cfg, &id, model)?;
            id
        }
        (Some(source), None) => match pinned_model(cfg, source)? {
            None if spec.fork_session => fork_session(cfg, source)?,
            None => source.to_string(),
            Some(pinned) if spec.fork_session => {
                return Err(PmError::Agent(format!(
                    "session {source} is pinned to model {pinned} and '{}' has no \
                     [agents.models] row; opencode cannot unpin a session, so a fork would \
                     keep that model — set the row, then fork",
                    ctx.agent
                )));
            }
            Some(pinned) => {
                notes.push(format!(
                    "session {source} is pinned to model {pinned}, which opencode cannot \
                     unpin; started a fresh session"
                ));
                create_session(cfg, ctx, spec.definition, None)?
            }
        },
        (None, _) => create_session(cfg, ctx, spec.definition, model.as_ref())?,
    };

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
    env.push((CONFIG_ENV.to_string(), path.to_string_lossy().into_owned()));
    Ok(PreLaunch {
        session_id: Some(session_id),
        env,
        env_remove: vec![CONFIG_CONTENT_ENV.to_string()],
        notes,
    })
}

/// The per-spawn config; `{}` when the spawn sets nothing.
fn render_config(
    spec: &SpawnSpec<'_>,
    cfg: &OpenCodeConfig,
    model: Option<&ModelRef<'_>>,
) -> Result<Value> {
    let mut config = serde_json::Map::new();
    if let (Some(row), Some(model)) = (spec.model, model) {
        config.insert("model".to_string(), json!(row));
        config.insert("enabled_providers".to_string(), json!([model.provider]));
    }

    let mut rules = Vec::new();
    if cfg.auto == Some(false) {
        for dir in spec.writable_dirs {
            rules.push(json!({
                "action": "external_directory",
                "resource": format!("{}/*", dir.display()),
                "effect": "allow",
            }));
        }
    }
    if let Some(row) = spec.permission_mode {
        match serde_json::from_str::<Value>(row) {
            Ok(Value::Array(own)) => rules.extend(own),
            _ => {
                return Err(PmError::Agent(format!(
                    "[agents.permissions] row for an opencode agent must be a JSON array of \
                     opencode permission rules, e.g. \
                     '[{{\"action\":\"edit\",\"resource\":\"*\",\"effect\":\"deny\"}}]'; got: {row}"
                )));
            }
        }
    }
    if !rules.is_empty() {
        config.insert("permissions".to_string(), Value::Array(rules));
    }

    Ok(Value::Object(config))
}

fn create_session(
    cfg: &OpenCodeConfig,
    ctx: &LaunchContext<'_>,
    definition: Option<&str>,
    model: Option<&ModelRef<'_>>,
) -> Result<String> {
    // The TUI reports its location by the resolved cwd.
    let directory = ctx
        .worktree
        .canonicalize()
        .unwrap_or_else(|_| ctx.worktree.to_path_buf());
    let mut body = json!({
        "location": {"directory": directory},
        "title": format!("pm:{}", ctx.agent),
    });
    if let Some(def) = definition {
        body["agent"] = json!(def);
    }
    if let Some(model) = model {
        body["model"] = model.to_json();
    }
    let body = body.to_string();
    session_id(&api(cfg, &["session.create", "--data", &body])?)
}

fn pin_model(cfg: &OpenCodeConfig, session: &str, model: &ModelRef<'_>) -> Result<()> {
    let param = format!("sessionID={session}");
    let body = json!({"model": model.to_json()}).to_string();
    api(
        cfg,
        &["session.switchModel", "--param", &param, "--data", &body],
    )?;
    Ok(())
}

/// The model pinned on `session`, as `<provider>/<model>`. A session that
/// has only ever run on the config's model has none.
fn pinned_model(cfg: &OpenCodeConfig, session: &str) -> Result<Option<String>> {
    let param = format!("sessionID={session}");
    let response = api(cfg, &["session.get", "--param", &param])?;
    let model = response.pointer("/data/model");
    let part = |key: &str| model.and_then(|m| m.get(key)).and_then(Value::as_str);
    Ok(part("providerID")
        .zip(part("id"))
        .map(|(provider, id)| format!("{provider}/{id}")))
}

fn fork_session(cfg: &OpenCodeConfig, source: &str) -> Result<String> {
    let param = format!("sessionID={source}");
    session_id(&api(
        cfg,
        &["session.fork", "--param", &param, "--data", "{}"],
    )?)
}

fn session_id(response: &Value) -> Result<String> {
    response
        .pointer("/data/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PmError::Agent(format!("opencode returned no session id: {response}")))
}

/// `opencode api <args>`, with stdin closed because `api` otherwise waits on
/// it.
fn api_command(cfg: &OpenCodeConfig, args: &[&str]) -> Command {
    let argv = argv(cfg, Some("api"), args);
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdin(Stdio::null());
    for key in SCRUBBED_ENV {
        command.env_remove(key);
    }
    command
}

fn api(cfg: &OpenCodeConfig, args: &[&str]) -> Result<Value> {
    let operation = args.first().copied().unwrap_or_default();
    let mut command = api_command(cfg, args);
    let out = command.output().map_err(|e| {
        PmError::Agent(format!(
            "could not run `{}` for {operation}: {e}",
            command.get_program().to_string_lossy()
        ))
    })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(PmError::Agent(format!(
            "opencode {operation} failed: {}",
            api_error(&stdout, &String::from_utf8_lossy(&out.stderr))
        )));
    }
    // A call that succeeds with nothing to return prints nothing.
    if stdout.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(stdout.trim()).map_err(|e| {
        PmError::Agent(format!(
            "opencode {operation} returned unreadable output ({e}): {}",
            stdout.trim()
        ))
    })
}

/// The `message` of the error object opencode prints on stdout, else
/// whatever it printed.
fn api_error(stdout: &str, stderr: &str) -> String {
    stdout
        .lines()
        .find_map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()?
                .get("message")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            format!("{} {}", stdout.trim(), stderr.trim())
                .trim()
                .to_string()
        })
}

/// A per-spawn file for one agent of one scope. Keyed by scope as well as
/// name: unlike the composed prompt, the content differs between two
/// features' agents of the same name.
fn spawn_file(project_root: &Path, scope: &str, agent: &str, extension: &str) -> Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    project_root.hash(&mut h);
    scope.hash(&mut h);
    let digest = h.finish();
    Ok(crate::notice::spawn_dir()?.join(format!("pm-opencode-{digest:x}-{agent}.{extension}")))
}

/// Where the plugin records why its loop stopped.
pub(super) fn trip_file(project_root: &Path, scope: &str, agent: &str) -> Result<PathBuf> {
    spawn_file(project_root, scope, agent, "tripped")
}

/// `(major, minor, patch)` from `opencode --version` output (`opencode v2.0.18`).
fn parse_version(output: &str) -> Option<(u32, u32, u32)> {
    let token = output.split_whitespace().last()?;
    let mut nums = token
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>().ok());
    Some((nums.next()??, nums.next()??, nums.next()??))
}

/// The installed version's raw string, or `None` when opencode can't be
/// run. `--version` starts no server, so it needs no `--standalone`.
fn installed_version(cfg: &OpenCodeConfig) -> Option<String> {
    let out = Command::new(binary(cfg))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether the installed opencode is at least [`MIN_VERSION`]; `None` when
/// it can't be probed.
pub(super) fn version_supported(cfg: &OpenCodeConfig) -> Option<bool> {
    Some(parse_version(&installed_version(cfg)?)? >= MIN_VERSION)
}

pub(super) fn unusable_reason(cfg: &OpenCodeConfig) -> Option<String> {
    let Some(found) = installed_version(cfg) else {
        return Some(format!(
            "`{}` could not be run; install opencode or set `[harness.opencode] binary`",
            binary(cfg)
        ));
    };
    match parse_version(&found) {
        Some(version) if version >= MIN_VERSION => None,
        _ => Some(format!(
            "installed opencode is `{found}`; pm's never-idle plugin needs {} or later",
            min_version_string()
        )),
    }
}

pub(super) fn min_version_string() -> String {
    let (a, b, c) = MIN_VERSION;
    format!("{a}.{b}.{c}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{fake_opencode_argv, fake_opencode_calls};

    fn cfg() -> OpenCodeConfig {
        OpenCodeConfig::default()
    }

    fn pre(session: &str) -> PreLaunch {
        PreLaunch {
            session_id: Some(session.to_string()),
            env: vec![
                (SESSION_ENV.to_string(), session.to_string()),
                (PROMPT_ENV.to_string(), "/x/base line.md".to_string()),
            ],
            env_remove: vec![CONFIG_CONTENT_ENV.to_string()],
            notes: Vec::new(),
        }
    }

    fn fake_opencode(dir: &Path, answer: &str, exit: i32) -> OpenCodeConfig {
        OpenCodeConfig {
            binary: Some(crate::testing::fake_opencode(dir, answer, exit)),
            ..Default::default()
        }
    }

    fn ctx<'a>(dir: &'a Path, agent: &'a str) -> LaunchContext<'a> {
        LaunchContext {
            project_root: dir,
            feature: "login",
            worktree: dir,
            agent,
        }
    }

    fn env_of<'a>(pre: &'a PreLaunch, key: &str) -> Option<&'a str> {
        pre.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn written_config(pre: &PreLaunch) -> Value {
        let path = env_of(pre, CONFIG_ENV).expect("every spawn names a config file");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The JSON body of an `api … --data <body>` call.
    fn body_of(call: &[String]) -> Value {
        let at = call.iter().position(|a| a == "--data").expect("--data");
        serde_json::from_str(&call[at + 1]).unwrap()
    }

    #[test]
    fn build_cmd_opens_the_prepared_session_standalone_and_auto() {
        let cmd = build_cmd(
            &SpawnSpec {
                definition: Some("reviewer"),
                prompt: Some("Stand by."),
                model: Some("local/qwen"),
                ..Default::default()
            },
            &cfg(),
            &pre("ses_1"),
        );
        // The definition, model and sentinel prompt never reach the command
        // line: the session already carries the agent and the model, and the
        // plugin arms without a first turn.
        assert_eq!(
            cmd,
            "'env' '-u' 'OPENCODE_CONFIG_CONTENT' 'PM_OPENCODE_SESSION=ses_1' \
             'PM_APPEND_PROMPT_FILE=/x/base line.md' 'opencode' '--standalone' '--auto' \
             '--session' 'ses_1'"
        );
    }

    #[test]
    fn build_cmd_quotes_a_session_id_the_shell_would_act_on() {
        let cmd = build_cmd(
            &SpawnSpec::default(),
            &cfg(),
            &PreLaunch {
                session_id: Some("ses_1; touch 'x' $(id)".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(
            cmd,
            "'env' 'opencode' '--standalone' '--auto' '--session' 'ses_1; touch '\\''x'\\'' $(id)'"
        );
    }

    #[test]
    fn build_cmd_drops_auto_only_when_configured_off() {
        let off = OpenCodeConfig {
            auto: Some(false),
            binary: Some("/opt/open code/bin/opencode".into()),
        };
        assert_eq!(
            build_cmd(&SpawnSpec::default(), &off, &PreLaunch::default()),
            "'env' '/opt/open code/bin/opencode' '--standalone'"
        );
        let on = OpenCodeConfig {
            auto: Some(true),
            ..Default::default()
        };
        assert_eq!(
            build_cmd(&SpawnSpec::default(), &on, &PreLaunch::default()),
            "'env' 'opencode' '--standalone' '--auto'"
        );
    }

    #[test]
    fn every_command_line_is_standalone() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);

        for spec in [
            SpawnSpec::default(),
            SpawnSpec {
                model: Some("local/qwen"),
                ..Default::default()
            },
            SpawnSpec {
                resume_session: Some("ses_old"),
                model: Some("local/qwen"),
                ..Default::default()
            },
            SpawnSpec {
                resume_session: Some("ses_old"),
                fork_session: true,
                model: Some("local/qwen"),
                ..Default::default()
            },
        ] {
            let before = fake_opencode_calls(dir.path()).len();
            let pre = pre_launch(&ctx(dir.path(), "reviewer"), &spec, &cfg).unwrap();
            let calls = fake_opencode_calls(dir.path());
            assert!(calls.len() > before, "{spec:?} made no call");
            for call in &calls[before..] {
                assert_eq!(&call[..2], ["api", "--standalone"], "{spec:?}");
            }
            let cmd = build_cmd(&spec, &cfg, &pre);
            assert!(
                cmd.contains("/opencode' '--standalone' '--auto' '--session' "),
                "{spec:?}: {cmd}"
            );
        }
    }

    #[test]
    fn pre_launch_creates_a_session_for_the_definition_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let worktree = project.join("login");
        std::fs::create_dir_all(&worktree).unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new","agent":"rev"}}"#, 0);

        let pre = pre_launch(
            &LaunchContext {
                project_root: &project,
                feature: "login",
                worktree: &worktree,
                agent: "frontend-rev",
            },
            &SpawnSpec {
                definition: Some("rev"),
                append_prompt_file: Some("/x/baseline.md"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();

        assert_eq!(pre.session_id.as_deref(), Some("ses_new"));
        assert_eq!(env_of(&pre, SESSION_ENV), Some("ses_new"));
        assert_eq!(env_of(&pre, PROMPT_ENV), Some("/x/baseline.md"));
        assert_eq!(
            env_of(&pre, TRIP_ENV),
            Some(
                trip_file(&project, "login", "frontend-rev")
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );

        let calls = fake_opencode_calls(dir.path());
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            &calls[0][..4],
            ["api", "--standalone", "session.create", "--data"]
        );
        assert_eq!(
            body_of(&calls[0]),
            json!({
                "location": {"directory": worktree.canonicalize().unwrap()},
                "title": "pm:frontend-rev",
                "agent": "rev",
            })
        );
    }

    #[test]
    fn a_spawn_never_runs_on_the_config_of_whoever_spawned_it() {
        // pm often runs inside an agent's shell, which carries that agent's
        // identity and opencode config.
        let command = api_command(&cfg(), &["session.create"]);
        let removed: Vec<&str> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .filter_map(|(key, _)| key.to_str())
            .collect();
        for key in [
            "PM_AGENT_NAME",
            SESSION_ENV,
            PROMPT_ENV,
            TRIP_ENV,
            CONFIG_ENV,
            CONFIG_CONTENT_ENV,
        ] {
            assert!(removed.contains(&key), "{key} reaches the api call");
        }

        // An agent with no row of its own still gets a config file, so an
        // inherited `OPENCODE_CONFIG` cannot apply, and the inline form is
        // removed outright.
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
        let pre = pre_launch(&ctx(dir.path(), "reviewer"), &SpawnSpec::default(), &cfg).unwrap();
        assert_eq!(written_config(&pre), json!({}));
        assert_eq!(pre.env_remove, [CONFIG_CONTENT_ENV]);
        let cmd = build_cmd(&SpawnSpec::default(), &cfg, &pre);
        assert!(
            cmd.starts_with("'env' '-u' 'OPENCODE_CONFIG_CONTENT' "),
            "{cmd}"
        );
        assert!(
            cmd.contains(&format!(
                "'OPENCODE_CONFIG={}'",
                env_of(&pre, CONFIG_ENV).unwrap()
            )),
            "{cmd}"
        );
    }

    #[test]
    fn pre_launch_vanilla_session_names_no_agent_and_no_model() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
        pre_launch(&ctx(dir.path(), "default"), &SpawnSpec::default(), &cfg).unwrap();
        let body = body_of(&fake_opencode_argv(dir.path()));
        assert!(body.get("agent").is_none(), "{body}");
        assert!(body.get("model").is_none(), "{body}");
    }

    #[test]
    fn a_model_row_is_pinned_on_the_session_and_limits_the_agent_to_its_provider() {
        // A model that only config names is one opencode replaces with its
        // default when it cannot resolve it; a pinned one fails the turn.
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
        let pre = pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec {
                model: Some("local/mlx-community/Qwen3.8-27B-4bit#high"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();

        assert_eq!(
            body_of(&fake_opencode_argv(dir.path()))["model"],
            json!({
                "providerID": "local",
                "id": "mlx-community/Qwen3.8-27B-4bit",
                "variant": "high",
            })
        );
        assert_eq!(
            written_config(&pre),
            json!({
                "model": "local/mlx-community/Qwen3.8-27B-4bit#high",
                "enabled_providers": ["local"],
            })
        );
    }

    #[test]
    fn a_model_row_opencode_could_not_pin_is_refused_before_any_call() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_new"}}"#, 0);
        for row in ["opus", "/opus", "anthropic/", "anthropic/opus#", ""] {
            let err = pre_launch(
                &ctx(dir.path(), "reviewer"),
                &SpawnSpec {
                    model: Some(row),
                    ..Default::default()
                },
                &cfg,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.ends_with(&format!(
                    "must be `<provider>/<model>[#variant]`; got: {row}"
                )),
                "{row}: {err}"
            );
        }
        assert!(fake_opencode_calls(dir.path()).is_empty());
    }

    #[test]
    fn pre_launch_resume_reuses_a_stored_session_that_is_not_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_stored"}}"#, 0);
        let pre = pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec {
                resume_session: Some("ses_stored"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();
        assert_eq!(pre.session_id.as_deref(), Some("ses_stored"));
        assert!(pre.notes.is_empty(), "{:?}", pre.notes);
        assert_eq!(
            fake_opencode_calls(dir.path()),
            [[
                "api",
                "--standalone",
                "session.get",
                "--param",
                "sessionID=ses_stored"
            ]]
        );
    }

    #[test]
    fn a_pinned_session_whose_row_was_removed_is_replaced_not_resumed() {
        // opencode has no way to unpin, and the agent must run on what
        // config says now.
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(
            dir.path(),
            r#"{"data":{"id":"ses_fresh","model":{"providerID":"local","id":"qwen","variant":"default"}}}"#,
            0,
        );
        let pre = pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec {
                definition: Some("reviewer"),
                resume_session: Some("ses_stored"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();

        assert_eq!(pre.session_id.as_deref(), Some("ses_fresh"));
        assert_eq!(env_of(&pre, SESSION_ENV), Some("ses_fresh"));
        assert_eq!(
            pre.notes,
            [
                "session ses_stored is pinned to model local/qwen, which opencode cannot unpin; \
              started a fresh session"
            ]
        );
        let calls = fake_opencode_calls(dir.path());
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0][2], "session.get");
        assert_eq!(calls[1][2], "session.create");
        let body = body_of(&calls[1]);
        assert_eq!(body["agent"], "reviewer");
        assert!(body.get("model").is_none(), "{body}");
        assert_eq!(written_config(&pre), json!({}));
    }

    #[test]
    fn a_pinned_session_whose_row_was_removed_is_not_forked() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(
            dir.path(),
            r#"{"data":{"id":"ses_x","model":{"providerID":"local","id":"qwen"}}}"#,
            0,
        );
        let err = pre_launch(
            &ctx(dir.path(), "reviewer-2"),
            &SpawnSpec {
                resume_session: Some("ses_source"),
                fork_session: true,
                ..Default::default()
            },
            &cfg,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("session ses_source is pinned to model local/qwen"),
            "{err}"
        );
        assert!(err.ends_with("set the row, then fork"), "{err}");
        let calls = fake_opencode_calls(dir.path());
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0][2], "session.get");
    }

    #[test]
    fn pre_launch_resume_pins_the_row_as_it_is_now() {
        // The session may predate the row, or carry the row's old value.
        let dir = tempfile::tempdir().unwrap();
        // What opencode prints for a call with nothing to return.
        let cfg = fake_opencode(dir.path(), "", 0);
        let pre = pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec {
                resume_session: Some("ses_stored"),
                model: Some("anthropic/claude-opus-5"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();
        assert_eq!(pre.session_id.as_deref(), Some("ses_stored"));
        let calls = fake_opencode_calls(dir.path());
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            &calls[0][..5],
            [
                "api",
                "--standalone",
                "session.switchModel",
                "--param",
                "sessionID=ses_stored"
            ]
        );
        assert_eq!(
            body_of(&calls[0]),
            json!({"model": {"providerID": "anthropic", "id": "claude-opus-5"}})
        );
    }

    #[test]
    fn pre_launch_fork_asks_opencode_for_a_new_session_and_pins_the_row_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(dir.path(), r#"{"data":{"id":"ses_fork"}}"#, 0);
        let pre = pre_launch(
            &ctx(dir.path(), "reviewer-2"),
            &SpawnSpec {
                resume_session: Some("ses_source"),
                fork_session: true,
                model: Some("local/qwen"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();
        assert_eq!(pre.session_id.as_deref(), Some("ses_fork"));
        let calls = fake_opencode_calls(dir.path());
        assert_eq!(
            calls[0],
            [
                "api",
                "--standalone",
                "session.fork",
                "--param",
                "sessionID=ses_source",
                "--data",
                "{}"
            ]
        );
        assert_eq!(
            &calls[1][2..5],
            ["session.switchModel", "--param", "sessionID=ses_fork"]
        );
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn pre_launch_reports_opencodes_own_error_message() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake_opencode(
            dir.path(),
            "{\"_tag\":\"InvalidRequestError\",\"message\":\"Cannot fork empty session: ses_x\",\
             \"kind\":\"empty_session\"}\nHTTP 400 Bad Request",
            1,
        );
        let err = pre_launch(
            &ctx(dir.path(), "reviewer-2"),
            &SpawnSpec {
                resume_session: Some("ses_x"),
                fork_session: true,
                model: Some("local/qwen"),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.ends_with("opencode session.fork failed: Cannot fork empty session: ses_x"),
            "{err}"
        );
    }

    #[test]
    fn pre_launch_errors_when_the_binary_is_missing_or_answers_without_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let missing = OpenCodeConfig {
            binary: Some(dir.path().join("nope").to_string_lossy().into_owned()),
            ..Default::default()
        };
        let err = pre_launch(
            &ctx(dir.path(), "reviewer"),
            &SpawnSpec::default(),
            &missing,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("could not run"), "{err}");

        for answer in [r#"{"data":{}}"#, ""] {
            let cfg = fake_opencode(dir.path(), answer, 0);
            let err = pre_launch(&ctx(dir.path(), "reviewer"), &SpawnSpec::default(), &cfg)
                .unwrap_err()
                .to_string();
            assert!(err.contains("returned no session id"), "{answer}: {err}");
        }
    }

    #[test]
    fn two_features_agents_of_one_name_get_a_config_file_each() {
        let root = Path::new("/proj");
        assert_ne!(
            spawn_file(root, "login", "reviewer", "json").unwrap(),
            spawn_file(root, "signup", "reviewer", "json").unwrap()
        );
    }

    #[test]
    fn render_config_is_empty_without_settings_under_auto() {
        let dirs = vec![PathBuf::from("/proj/.pm")];
        let spec = SpawnSpec {
            writable_dirs: &dirs,
            ..Default::default()
        };
        assert_eq!(render_config(&spec, &cfg(), None).unwrap(), json!({}));
    }

    #[test]
    fn render_config_passes_the_permission_row_through_as_rules() {
        let spec = SpawnSpec {
            permission_mode: Some(
                r#"[{"action":"edit","resource":"*","effect":"deny"},{"action":"made-up"}]"#,
            ),
            ..Default::default()
        };
        assert_eq!(
            render_config(&spec, &cfg(), None).unwrap(),
            json!({
                "permissions": [
                    {"action": "edit", "resource": "*", "effect": "deny"},
                    {"action": "made-up"},
                ],
            })
        );
    }

    #[test]
    fn render_config_without_auto_allows_pm_state_dirs_ahead_of_the_row() {
        let dirs = vec![PathBuf::from("/proj/.pm"), PathBuf::from("/proj/main/.git")];
        let spec = SpawnSpec {
            permission_mode: Some(
                r#"[{"action":"external_directory","resource":"*","effect":"deny"}]"#,
            ),
            writable_dirs: &dirs,
            ..Default::default()
        };
        let off = OpenCodeConfig {
            auto: Some(false),
            ..Default::default()
        };
        // Last match wins in opencode, so the row can still override pm's
        // allows.
        assert_eq!(
            render_config(&spec, &off, None).unwrap(),
            json!({"permissions": [
                {"action": "external_directory", "resource": "/proj/.pm/*", "effect": "allow"},
                {"action": "external_directory", "resource": "/proj/main/.git/*", "effect": "allow"},
                {"action": "external_directory", "resource": "*", "effect": "deny"},
            ]})
        );
    }

    #[test]
    fn render_config_rejects_a_row_that_is_not_a_rule_list() {
        for row in ["acceptEdits", r#"{"action":"edit"}"#, "[unterminated"] {
            let spec = SpawnSpec {
                permission_mode: Some(row),
                ..Default::default()
            };
            let err = render_config(&spec, &cfg(), None).unwrap_err().to_string();
            assert!(err.contains("must be a JSON array"), "{row}: {err}");
            assert!(err.ends_with(&format!("got: {row}")), "{row}: {err}");
        }
    }

    #[test]
    fn unusable_reason_names_a_missing_or_old_binary() {
        let dir = tempfile::tempdir().unwrap();
        let missing = OpenCodeConfig {
            binary: Some(dir.path().join("nope").to_string_lossy().into_owned()),
            ..Default::default()
        };
        let reason = unusable_reason(&missing).unwrap();
        assert!(reason.contains("could not be run"), "{reason}");
        assert_eq!(version_supported(&missing), None);

        let old = fake_opencode(dir.path(), "opencode v2.0.17", 0);
        assert_eq!(
            unusable_reason(&old).unwrap(),
            "installed opencode is `opencode v2.0.17`; pm's never-idle plugin needs 2.0.18 or \
             later"
        );
        assert_eq!(version_supported(&old), Some(false));

        let current = fake_opencode(dir.path(), "opencode v2.0.18", 0);
        assert_eq!(unusable_reason(&current), None);
        assert_eq!(version_supported(&current), Some(true));
    }

    #[test]
    fn parse_version_reads_opencode_output() {
        assert_eq!(parse_version("opencode v2.0.18"), Some((2, 0, 18)));
        assert_eq!(parse_version("opencode v2.1.0-beta.2\n"), Some((2, 1, 0)));
        assert_eq!(parse_version("2.0.18"), Some((2, 0, 18)));
        assert_eq!(parse_version("opencode"), None);
        assert!(parse_version("opencode v2.0.17").unwrap() < MIN_VERSION);
        assert!(parse_version("opencode v2.0.18").unwrap() >= MIN_VERSION);
    }
}
