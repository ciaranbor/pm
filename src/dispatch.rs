use std::os::unix::process::CommandExt;
use std::process::Stdio;

use clap::CommandFactory;

use crate::cli::*;
use pm::commands;
use pm::commands::attention::AgentState;
use pm::commands::harness_export::ExportParams;
use pm::commands::harness_migrate::MigrateParams;
use pm::commands::tmux_push::AgentWindow;
use pm::error::PmError;
use pm::harness::{Harness, Probe};
use pm::state::paths;
use pm::state::project::GlobalConfig;
use pm::tmux;

mod serve;

fn resolve_feature_name(
    name: Option<String>,
    project_root: &std::path::Path,
) -> pm::error::Result<String> {
    name.or_else(|| paths::detect_feature_from_cwd(project_root, &std::env::current_dir().ok()?))
        .ok_or(PmError::NotInFeatureWorktree)
}

/// The agent this command runs in, if any.
fn running_agent() -> Option<String> {
    std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
}

/// Resolve the current scope: feature name if in a feature worktree,
/// "main" if in the main worktree, error otherwise.
fn resolve_scope(project_root: &std::path::Path) -> pm::error::Result<String> {
    paths::resolve_scope_from(project_root, &std::env::current_dir()?)
}

/// Validate that a scope name refers to an existing scope ("main" or a known feature).
fn validate_scope(project_root: &std::path::Path, scope: &str) -> pm::error::Result<()> {
    if scope == "main" {
        return Ok(());
    }
    let features_dir = paths::features_dir(project_root);
    let state_file = features_dir.join(format!("{scope}.toml"));
    if state_file.exists() {
        Ok(())
    } else {
        Err(PmError::FeatureNotFound(scope.to_string()))
    }
}

/// Resolve scope with an optional override flag. If `scope_flag` is Some,
/// validates it and returns it; otherwise auto-detects from CWD.
fn resolve_scope_with_flag(
    project_root: &std::path::Path,
    scope_flag: Option<String>,
) -> pm::error::Result<String> {
    match scope_flag {
        Some(s) => {
            validate_scope(project_root, &s)?;
            Ok(s)
        }
        None => resolve_scope(project_root),
    }
}

/// Read a message body from an explicit argument or stdin. Used by both
/// `pm msg send` and `pm msg reply`.
fn read_message_body(message: Option<String>) -> pm::error::Result<String> {
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

/// Spawn a new project's `main` agent. The project is set up by now, so a
/// failure is reported with the command that retries it, not returned.
fn report_main_spawn(project_root: &std::path::Path, server: Option<&str>) {
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
fn report_agent_op_results(
    results: Vec<pm::error::Result<String>>,
    op_label: &str,
) -> pm::error::Result<()> {
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
fn report_restart_all(
    scopes: &[commands::agent_restart_all::Scope],
    unread: Vec<commands::agent_restart_all::Report>,
    select: commands::agent_restart_all::Select,
    force: bool,
    server: Option<&str>,
) -> pm::error::Result<()> {
    use commands::agent_restart_all::Outcome;
    let mut done = commands::agent_restart_all::restart_all(scopes, select, force, server)?;
    done.confirm_launches(server);
    let mut sweep = done.sweep();
    sweep.reports.splice(0..0, unread);
    for report in &sweep.reports {
        match report.outcome {
            Outcome::Failed(_) => eprintln!("{report}"),
            _ => println!("{report}"),
        }
    }
    println!("{}", sweep.summary());
    let failed = sweep.count(|o| matches!(o, Outcome::Failed(_))) > 0;
    std::io::Write::flush(&mut std::io::stdout())?;
    done.finish(server);
    push();
    if failed {
        return Err(PmError::Agent("some agents failed to restart".to_string()));
    }
    Ok(())
}

fn restart_select(stale: bool) -> commands::agent_restart_all::Select {
    if stale {
        commands::agent_restart_all::Select::Stale
    } else {
        commands::agent_restart_all::Select::All
    }
}

/// Print what a send did, reporting a respawned recipient only once its
/// harness has stayed up. The message is queued either way, so a failed
/// launch is a warning.
fn report_sent(
    project_root: &std::path::Path,
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

/// Print each launch that failed; an error saying how many, if any did.
fn report_failed_launches(
    failed: &[commands::launch_check::FailedLaunch],
) -> pm::error::Result<()> {
    for failure in failed {
        eprintln!("error: {}", failure.message());
    }
    match failed.len() {
        0 => Ok(()),
        n => Err(PmError::Agent(format!(
            "{n} agent{} failed to launch",
            plural(n)
        ))),
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

/// The tmux server every command in this process targets: `PM_TMUX_SERVER`
/// as a `-L` socket name, or the default server when unset or empty.
fn tmux_server_from_env() -> Option<String> {
    std::env::var("PM_TMUX_SERVER")
        .ok()
        .filter(|s| !s.is_empty())
}

/// The root of the project named `project`, or else of the one the cwd is in.
fn project_root(
    projects_dir: &std::path::Path,
    project: Option<&str>,
) -> pm::error::Result<std::path::PathBuf> {
    match project {
        Some(name) => Ok(pm::state::project::ProjectEntry::load(projects_dir, name)?.root_path()),
        None => paths::find_project_root(&std::env::current_dir()?),
    }
}

/// Leave the user in `session` rather than detached.
fn connect(server: Option<&str>, session: &str) {
    let tmux_env = std::env::var("TMUX").ok();
    if let Err(e) = tmux::connect_session(server, session, tmux_env.as_deref()) {
        eprintln!("warning: could not connect to {session}: {e}");
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Push the tmux refresh, killing first the session this process runs in
/// when the command left it to be killed last (`own`).
fn finish_in_own_session(
    server: Option<&str>,
    own: Option<tmux::OwnSession>,
) -> pm::error::Result<()> {
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let killed = own.map_or(Ok(()), |own| own.kill(server));
    push();
    killed
}

/// Bring pm's tmux options up to date with a change this command made, in
/// a background `pm tmux push` the command neither waits for nor fails on.
fn push() {
    let Ok(pm) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(pm)
        .args(["tmux", "push"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// The window of `agent`, run in the pane this command runs in.
fn agent_window(server: Option<&str>, agent: &str) -> Option<AgentWindow> {
    let pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())?;
    Some(AgentWindow::new(server, &pane, agent))
}

/// Write this agent's own window options now, as a hook changing its state
/// does, ahead of the push that refreshes the rest.
fn publish(state: AgentState, unread: u32) {
    if let Some(mut window) =
        running_agent().and_then(|agent| agent_window(tmux_server_from_env().as_deref(), &agent))
    {
        window.publish(state, unread);
    }
}

/// Hook handlers hand back a process exit code; a non-zero one is the
/// handler's whole answer to the harness and must reach it verbatim.
fn exit_unless_ok(code: i32) -> pm::error::Result<()> {
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

pub fn run(cli: Cli) -> pm::error::Result<()> {
    let server = tmux_server_from_env();
    let server = server.as_deref();
    match cli.command {
        Commands::Init { path, git, no_main } => {
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
        Commands::Register {
            path,
            name,
            r#move,
            no_main,
        } => {
            let projects_dir = paths::global_projects_dir()?;
            let root = commands::register::register(
                &path,
                name.as_deref(),
                &projects_dir,
                r#move,
                server,
                None,
            )?;
            if !no_main {
                report_main_spawn(&root, server);
            }
            Ok(())
        }
        Commands::List => {
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
        Commands::Open { all: true, .. } => {
            use commands::open::ProjectOpen;
            let projects_dir = paths::global_projects_dir()?;
            let mut changed = false;
            let mut outcomes =
                commands::open::open_all(&projects_dir, server, |name, outcome| match outcome {
                    ProjectOpen::Opened(r) => {
                        changed |= r.changed();
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
        Commands::Open { project, .. } => {
            let projects_dir = paths::global_projects_dir()?;
            let project_root = project_root(&projects_dir, project.as_deref())?;
            let mut result = commands::open::open(&project_root, &projects_dir, server)?;
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
        Commands::Harness(cmd) | Commands::Claude(cmd) => dispatch_harness(cmd),
        Commands::Close { project, all } => {
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
        Commands::Agent(AgentCommands::Restart {
            all: true,
            global: true,
            stale,
            force,
            ..
        }) => {
            let (scopes, unread) =
                commands::agent_restart_all::global_scopes(&paths::global_projects_dir()?)?;
            report_restart_all(&scopes, unread, restart_select(stale), force, server)
        }
        Commands::Agent(agent_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match agent_cmd {
                AgentCommands::Spawn {
                    name,
                    agent_definition,
                    context,
                    scope,
                } => {
                    let feature = resolve_scope_with_flag(&project_root, scope)?;
                    // `--context -` reads the brief from stdin; any other
                    // value is treated as a literal string (no file resolution).
                    let context = commands::feat_new::resolve_stdin_context(context.as_deref())?;
                    if let Some(agent_name) = name {
                        let (outcome, msg, _) = commands::agent_spawn::agent_spawn(
                            &project_root,
                            &feature,
                            &agent_name,
                            agent_definition.as_deref(),
                            context.as_deref(),
                            server,
                        )?;
                        if outcome.is_new_window()
                            && let Err(e) = commands::launch_check::check(
                                &project_root,
                                &feature,
                                &agent_name,
                                server,
                            )
                        {
                            push();
                            return Err(e);
                        }
                        println!("{msg}");
                    } else {
                        if agent_definition.is_some() {
                            return Err(PmError::Agent(
                                "--agent requires a positional NAME (the display name to register under)"
                                    .to_string(),
                            ));
                        }
                        let mut result = commands::agent_spawn::agent_spawn_all(
                            &project_root,
                            &feature,
                            server,
                        )?;
                        result.confirm_launches(&project_root, &feature, server);
                        for msg in &result.successes {
                            println!("{msg}");
                        }
                        for err in &result.errors {
                            eprintln!("error: {err}");
                        }
                    }
                    push();
                    Ok(())
                }
                AgentCommands::Stop { names, scope } => {
                    let target_scope = resolve_scope_with_flag(&project_root, scope)?;
                    let results = commands::agent_stop::agent_stop_many(
                        &project_root,
                        &target_scope,
                        &names,
                        server,
                    );
                    push();
                    report_agent_op_results(results, "stop")
                }
                AgentCommands::Delete { names, scope } => {
                    let target_scope = resolve_scope_with_flag(&project_root, scope)?;
                    let results = commands::agent_delete::agent_delete_many(
                        &project_root,
                        &target_scope,
                        &names,
                        server,
                    );
                    push();
                    report_agent_op_results(results, "delete")
                }
                AgentCommands::Restart {
                    names,
                    all,
                    stale,
                    force,
                    scope,
                    ..
                } => {
                    let target_scope = resolve_scope_with_flag(&project_root, scope)?;
                    if all {
                        let scope =
                            commands::agent_restart_all::Scope::of(&project_root, &target_scope)?;
                        return report_restart_all(
                            &[scope],
                            Vec::new(),
                            restart_select(stale),
                            force,
                            server,
                        );
                    }
                    let mut restarted = commands::agent_restart::agent_restart_many(
                        &project_root,
                        &target_scope,
                        &names,
                        force,
                        true,
                        server,
                    );
                    restarted.confirm_launches(&project_root, &target_scope, server);
                    let reported =
                        report_agent_op_results(std::mem::take(&mut restarted.results), "restart");
                    std::io::Write::flush(&mut std::io::stdout())?;
                    restarted.finish(server);
                    push();
                    reported
                }
                AgentCommands::List { active, scope } => {
                    let feature = resolve_scope_with_flag(&project_root, scope)?;
                    let lines = commands::agent_list::agent_list(&project_root, &feature, active)?;
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                AgentCommands::Fork {
                    source,
                    name,
                    scope,
                } => {
                    let feature = resolve_scope_with_flag(&project_root, scope)?;
                    let msg = commands::agent_fork::agent_fork(
                        &project_root,
                        &feature,
                        &source,
                        &name,
                        server,
                    )?;
                    let launched =
                        commands::launch_check::check(&project_root, &feature, &name, server);
                    push();
                    launched?;
                    println!("{msg}");
                    Ok(())
                }
            }
        }
        Commands::Msg(msg_cmd) => {
            let (project_root, feature) = paths::agent_scope()?;
            match msg_cmd {
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
                        let sender_project_config =
                            pm::state::project::ProjectConfig::load(&pm_dir)?;
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
                    let sent = commands::msg_reply::msg_reply(
                        &project_root,
                        &feature,
                        &sender,
                        &message,
                        server,
                    )?;
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
        Commands::Feat(FeatCommands::Status {
            all: true, json, ..
        }) => {
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
        Commands::Feat(FeatCommands::List { all: true }) => {
            let lines = commands::feat_list::feat_list_all(&paths::global_projects_dir()?)?;
            if lines.is_empty() {
                println!("No projects");
            }
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::Feat(feat_cmd) => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            let projects_dir = paths::global_projects_dir()?;
            match feat_cmd {
                FeatCommands::New {
                    name,
                    feature_name,
                    context,
                    base,
                    workflow,
                } => {
                    let feat_name =
                        commands::feat_new::feat_new(&commands::feat_new::FeatNewParams {
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
                    let failed =
                        commands::launch_check::confirm_scope(&project_root, &feat_name, server);
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
                    let failed =
                        commands::launch_check::confirm_scope(&project_root, &feat_name, server);
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
                    let lines =
                        commands::feat_info::feat_info(&project_root, &projects_dir, &name)?;
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
                    let request = commands::feat_status::request(
                        &project_root,
                        status,
                        name,
                        reason.is_some(),
                    )?;
                    let (status, name) = match request {
                        Request::Set { .. } if json => {
                            return Err(pm::error::PmError::SafetyCheck(
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
                    let feature_name =
                        commands::feat_review::feat_review(&project_root, &pr, server)?;
                    println!("Created review feature '{feature_name}'");
                    push();
                    Ok(())
                }
                FeatCommands::Sync { name } => {
                    let name = name.or_else(|| {
                        paths::detect_feature_from_cwd(
                            &project_root,
                            &std::env::current_dir().ok()?,
                        )
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
        Commands::Delete {
            project,
            force,
            yes,
        } => {
            let projects_dir = paths::global_projects_dir()?;
            let project_root = project_root(&projects_dir, project.as_deref())?;
            let (project_name, own) =
                commands::delete::delete(&project_root, &projects_dir, force, yes, server)?;
            println!("Deleted project '{project_name}'");
            finish_in_own_session(server, own)
        }
        Commands::Notes { project } => {
            let projects_dir = paths::global_projects_dir()?;
            let project_root = project_root(&projects_dir, project.as_deref())?;
            commands::notes::edit(&project_root)
        }
        Commands::Status { project } => {
            let projects_dir = paths::global_projects_dir()?;
            let project_root = project_root(&projects_dir, project.as_deref())?;
            let lines = commands::status::status(&project_root, &projects_dir, server)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::Doctor { fix, project } => {
            let projects_dir = paths::global_projects_dir()?;
            let project_root = project_root(&projects_dir, project.as_deref())?;
            let lines =
                commands::doctor::doctor(&project_root, &projects_dir, fix, server)?.lines();
            println!("pm {}", pm::version::VERSION);
            for line in lines {
                println!("{line}");
            }
            if fix {
                push();
            }
            Ok(())
        }
        Commands::Upgrade { all, dry_run } => {
            let lines = commands::upgrade::upgrade(all, dry_run, server)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::Restore { projects, imports } => {
            let messages = commands::restore::restore(&projects, &imports, server)?;
            for msg in messages {
                println!("{msg}");
            }
            push();
            Ok(())
        }
        Commands::Migrate(MigrateCommands::Check {
            projects,
            verbose,
            json,
        }) => {
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
        Commands::SelfUpdate { force } => {
            let lines = commands::self_update::self_update(force)?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Commands::State(state_cmd) => match state_cmd {
            StateCommands::Init { global, remote } => {
                let msg = if global {
                    commands::state_cmd::global_init_with_remote(remote.as_deref())?
                } else {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    commands::state_cmd::init_with_remote(&project_root, remote.as_deref())?
                };
                println!("{msg}");
                Ok(())
            }
            StateCommands::Remote { url, global } => {
                let msg = if global {
                    let u = url.ok_or_else(|| {
                        PmError::Git(
                            "--global requires a URL (interactive mode not supported for global registry)".to_string(),
                        )
                    })?;
                    commands::state_cmd::global_remote(&u)?
                } else {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    commands::state_cmd::remote(&project_root, url.as_deref())?
                };
                println!("{msg}");
                Ok(())
            }
            StateCommands::Push { global } => {
                let msg = if global {
                    commands::state_cmd::global_push()?
                } else {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    commands::state_cmd::push(&project_root)?
                };
                println!("{msg}");
                Ok(())
            }
            StateCommands::Pull { global } => {
                let msg = if global {
                    commands::state_cmd::global_pull()?
                } else {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    commands::state_cmd::pull(&project_root)?
                };
                println!("{msg}");
                Ok(())
            }
            StateCommands::Status { global } => {
                let msg = if global {
                    commands::state_cmd::global_status()?
                } else {
                    let project_root = paths::find_project_root(&std::env::current_dir()?)?;
                    commands::state_cmd::status(&project_root)?
                };
                println!("{msg}");
                Ok(())
            }
            StateCommands::Backfill => {
                let messages = commands::state_cmd::backfill()?;
                for msg in messages {
                    println!("{msg}");
                }
                Ok(())
            }
        },
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "pm", &mut std::io::stdout());
            Ok(())
        }
        Commands::Serve { command, port } => serve::dispatch_serve(command, port, server),
        Commands::Tmux(TmuxCommands::Refresh) => {
            commands::tmux_refresh::refresh(&paths::global_projects_dir()?, server)
        }
        Commands::Tmux(TmuxCommands::Init) => commands::tmux_init::init(server),
        Commands::Tmux(TmuxCommands::Jump { client, target }) => {
            commands::tmux_jump::jump(&paths::global_projects_dir()?, server, &client, &target)
        }
        Commands::Tmux(TmuxCommands::Push) => {
            commands::serve::wake(&pm::state::devices::Devices::path(
                &paths::global_config_dir()?,
            ));
            commands::tmux_push::push(&paths::global_projects_dir()?, server)
        }
        Commands::Tmux(TmuxCommands::Watch) => {
            commands::tmux_watch::watch(&paths::global_projects_dir()?, server)
        }
        Commands::Workflow(workflow_cmd) => {
            // `install`/`uninstall`/`list` act on the global tier, so they
            // work outside a project too; `show` needs the feature's scope.
            let project_root = optional_project_root()?;
            match workflow_cmd {
                WorkflowCommands::Show => {
                    let project_root = project_root.ok_or(pm::error::PmError::NotInProject)?;
                    let scope = resolve_scope(&project_root)?;
                    match commands::workflow::show(&project_root, &scope)? {
                        Some(body) => {
                            // Use print! (not println!) to avoid adding a
                            // trailing blank line — workflow.md already
                            // ends in a newline.
                            print!("{body}");
                            if !body.ends_with('\n') {
                                println!();
                            }
                        }
                        None => {
                            println!("No workflow active for this feature.");
                        }
                    }
                    Ok(())
                }
                WorkflowCommands::List => {
                    let out = commands::workflow::list_rows(project_root.as_deref())?;
                    // Print rows to stdout (the normal listing).
                    if out.rows.is_empty() {
                        println!(
                            "No workflows installed. Run `pm upgrade` to install bundled workflows."
                        );
                    } else {
                        for line in &out.rows {
                            println!("{line}");
                        }
                    }
                    // Print warnings to stderr so they don't pollute pipe
                    // consumers but still surface broken workflows.
                    for w in &out.warnings {
                        eprintln!("{w}");
                    }
                    Ok(())
                }
            }
        }
    }
}

/// The current project root, or `None` when the caller isn't inside a
/// project — used by commands that also work outside one. Only that case is
/// swallowed; a genuine I/O failure still propagates.
fn optional_project_root() -> pm::error::Result<Option<std::path::PathBuf>> {
    match paths::find_project_root(&std::env::current_dir()?) {
        Ok(root) => Ok(Some(root)),
        Err(pm::error::PmError::NotInProject) => Ok(None),
        Err(e) => Err(e),
    }
}

fn dispatch_harness(cmd: HarnessCommands) -> pm::error::Result<()> {
    match cmd {
        HarnessCommands::Settings { harness, command } => {
            commands::harness_settings::settings_files(harness)?;
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            match command {
                HarnessSettingsCommands::List { name } => {
                    let scope = match name {
                        Some(n) => n,
                        None => resolve_scope(&project_root)?,
                    };
                    let (label, lines) = if scope == "main" {
                        (
                            "main".to_string(),
                            commands::harness_settings::list_main(&project_root, harness)?,
                        )
                    } else {
                        let lines =
                            commands::harness_settings::list(&project_root, &scope, harness)?;
                        (scope, lines)
                    };
                    if lines.is_empty() {
                        println!("No settings files found for '{label}'");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Ok(())
                }
                HarnessSettingsCommands::Push { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::harness_settings::push(&project_root, &name, harness)?;
                    println!("Pushed settings from feature '{name}' to main");
                    Ok(())
                }
                HarnessSettingsCommands::Pull { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    for line in commands::harness_settings::pull(&project_root, &name, harness)? {
                        println!("{line}");
                    }
                    Ok(())
                }
                HarnessSettingsCommands::Diff { name } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    let lines = commands::harness_settings::diff(&project_root, &name, harness)?;
                    if lines.is_empty() {
                        println!("No differences");
                    } else {
                        for line in lines {
                            println!("{line}");
                        }
                    }
                    Ok(())
                }
                HarnessSettingsCommands::Merge { name, ours } => {
                    let name = resolve_feature_name(name, &project_root)?;
                    commands::harness_settings::merge(&project_root, &name, ours, harness)?;
                    println!("Merged settings from feature '{name}' into main");
                    Ok(())
                }
            }
        }
        HarnessCommands::Skills(skills_cmd) => match skills_cmd {
            HarnessSkillsCommands::List => {
                let project_root = optional_project_root()?;
                let lines = commands::skills::skills_list(project_root.as_deref())?;
                for line in lines {
                    println!("{line}");
                }
                Ok(())
            }
            HarnessSkillsCommands::Install { name } => {
                for msg in commands::skills::skills_install(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
            HarnessSkillsCommands::Uninstall { name, all } => {
                if name.is_none() && !all {
                    eprintln!("Provide a skill name or use --all to uninstall all");
                    std::process::exit(1);
                }
                for msg in commands::skills::skills_uninstall(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
        },
        HarnessCommands::Agents(agents_cmd) => match agents_cmd {
            HarnessAgentsCommands::List => {
                let project_root = optional_project_root()?;
                let lines = commands::skills::agents_list(project_root.as_deref())?;
                for line in lines {
                    println!("{line}");
                }
                Ok(())
            }
            HarnessAgentsCommands::Install { name } => {
                for msg in commands::skills::agents_install(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
            HarnessAgentsCommands::Uninstall { name, all } => {
                if name.is_none() && !all {
                    eprintln!("Provide an agent name or use --all to uninstall all");
                    std::process::exit(1);
                }
                for msg in commands::skills::agents_uninstall(name.as_deref())? {
                    println!("{msg}");
                }
                Ok(())
            }
        },
        HarnessCommands::Hooks(hooks_cmd) => match hooks_cmd {
            HarnessHooksCommands::Install => {
                let project_root = optional_project_root()?;
                let msg = commands::hooks_install::install(project_root.as_deref())?;
                println!("{msg}");
                Ok(())
            }
            HarnessHooksCommands::Stop { harness } => {
                let mut window = running_agent()
                    .and_then(|agent| agent_window(tmux_server_from_env().as_deref(), &agent));
                exit_unless_ok(commands::hooks_stop::stop(harness, &mut |state, unread| {
                    if let Some(window) = window.as_mut() {
                        window.publish(state, unread);
                    }
                    if state != AgentState::Busy {
                        push();
                    }
                }))
            }
            HarnessHooksCommands::SessionStart => {
                let code = commands::hooks_session_start::session_start();
                // The harness is running now, so its window no longer reads
                // as dead.
                if running_agent().is_some() {
                    push();
                }
                exit_unless_ok(code)
            }
            HarnessHooksCommands::UserPrompt => {
                exit_unless_ok(commands::hooks_user_prompt::user_prompt(|unread| {
                    publish(AgentState::Busy, unread);
                    push();
                }))
            }
            HarnessHooksCommands::Waiting { harness } => exit_unless_ok(
                commands::hooks_waiting::waiting(harness, |state, unread| {
                    publish(state, unread);
                    push();
                }),
            ),
            HarnessHooksCommands::Dialog { harness } => {
                exit_unless_ok(commands::hooks_dialog::dialog(harness, push))
            }
        },
        HarnessCommands::Pull { name, dry_run } => {
            let project_root = paths::find_project_root(&std::env::current_dir()?)?;
            let name = resolve_feature_name(name, &project_root)?;
            let pulled = commands::seed::pull(&project_root, &name, dry_run)?;
            if pulled.written.is_empty() && pulled.removed.is_empty() {
                println!("Feature '{name}' is up to date with main");
            }
            let verb = if dry_run { "Would write" } else { "Wrote" };
            for file in pulled.written {
                println!("{verb} {name}/{}", file.display());
            }
            let verb = if dry_run { "Would remove" } else { "Removed" };
            for file in pulled.removed {
                println!(
                    "{verb} {name}/{}: the branch deleted its skill",
                    file.display()
                );
            }
            if dry_run {
                for file in pulled.deleted {
                    println!(
                        "Would skip {name}/{}: the branch deleted it",
                        file.display()
                    );
                }
            }
            Ok(())
        }
        HarnessCommands::Migrate { from, harness } => {
            let cwd = std::env::current_dir()?;
            let project_root = optional_project_root()?;
            let messages = commands::harness_migrate::migrate(&MigrateParams {
                harness,
                from: &from,
                to: &cwd,
                project_root: project_root.as_deref(),
                home: &paths::home_dir()?,
                global: &GlobalConfig::load_or_default().harness,
                tmux_server: tmux_server_from_env().as_deref(),
            })?;
            for msg in messages {
                println!("{msg}");
            }
            Ok(())
        }
        HarnessCommands::Export {
            all,
            output,
            harness,
        } => {
            let project_root = if all {
                None
            } else {
                Some(paths::find_project_root(&std::env::current_dir()?)?)
            };
            let (_, messages) = commands::harness_export::export(&ExportParams {
                harness,
                project_root: project_root.as_deref(),
                projects_dir: &paths::global_projects_dir()?,
                all,
                output: output.as_deref(),
                home: &paths::home_dir()?,
                global: &GlobalConfig::load_or_default().harness,
            })?;
            for msg in messages {
                println!("{msg}");
            }
            Ok(())
        }
        HarnessCommands::Import { tarball, harness } => {
            let report = commands::harness_import::import(
                harness,
                &tarball,
                &[],
                &paths::global_projects_dir()?,
                &paths::home_dir()?,
                &GlobalConfig::load_or_default().harness,
            )?;
            for msg in report.messages {
                println!("{msg}");
            }
            Ok(())
        }
        HarnessCommands::List => {
            for h in Harness::SUPPORTED {
                if *h == Harness::default() {
                    println!("{h} (default)");
                } else {
                    println!("{h}");
                }
            }
            Ok(())
        }
        HarnessCommands::Probe { harness } => {
            println!(
                "{}",
                commands::doctor::probe_line(harness, optional_project_root()?.as_deref())
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_feature_state(root: &std::path::Path, name: &str) {
        let feat_dir = root.join(".pm").join("features");
        std::fs::create_dir_all(&feat_dir).unwrap();
        std::fs::write(feat_dir.join(format!("{name}.toml")), "").unwrap();
    }

    #[test]
    fn validate_scope_accepts_main() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        assert!(validate_scope(root, "main").is_ok());
    }

    #[test]
    fn validate_scope_accepts_existing_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");

        assert!(validate_scope(root, "login").is_ok());
    }

    #[test]
    fn validate_scope_rejects_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = validate_scope(root, "nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn resolve_scope_with_flag_uses_override() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        create_feature_state(root, "login");

        let scope = resolve_scope_with_flag(root, Some("login".to_string())).unwrap();
        assert_eq!(scope, "login");
    }

    #[test]
    fn resolve_scope_with_flag_rejects_invalid_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".pm")).unwrap();

        let result = resolve_scope_with_flag(root, Some("bogus".to_string()));
        assert!(result.is_err());
    }

    // --- parse_agent_at_scope ---

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
