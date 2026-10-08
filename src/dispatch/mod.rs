//! Route a parsed command line to its handler. Each command group has a
//! submodule; the handlers in `commands/` do the work, and these print it.

use clap::CommandFactory;

use crate::cli::*;
use pm::commands;
use pm::state::paths;

mod agent;
mod feat;
mod harness;
mod maintenance;
mod msg;
mod project;
mod report;
mod scope;
mod serve;
mod state;
mod window;
mod workflow;

pub fn run(cli: Cli) -> pm::error::Result<()> {
    let server = window::tmux_server_from_env();
    let server = server.as_deref();
    match cli.command {
        Commands::Init { path, git, no_main } => project::init(path, git, no_main, server),
        Commands::Register {
            path,
            name,
            r#move,
            no_main,
        } => project::register(path, name, r#move, no_main, server),
        Commands::List => project::list(),
        Commands::Open { all: true, .. } => project::open_all(server),
        Commands::Open { project, .. } => project::open(project, server),
        Commands::Close { project, all } => project::close(project, all, server),
        Commands::Delete {
            project,
            force,
            yes,
        } => project::delete(project, force, yes, server),
        Commands::Notes { project } => project::notes(project),
        Commands::Status { project } => project::status(project, server),
        Commands::Harness(cmd) | Commands::Claude(cmd) => harness::run(cmd),
        Commands::Agent(cmd) => agent::run(cmd, server),
        Commands::Msg(cmd) => msg::run(cmd, server),
        Commands::Feat(cmd) => feat::run(cmd, server),
        Commands::Doctor { fix, project } => maintenance::doctor(fix, project, server),
        Commands::Upgrade { all: _, dry_run } => maintenance::upgrade(dry_run, server),
        Commands::Restore { projects, imports } => maintenance::restore(projects, imports, server),
        Commands::Migrate(MigrateCommands::Check {
            projects,
            verbose,
            json,
        }) => maintenance::migrate_check(projects, verbose, json, server),
        Commands::SelfUpdate { force } => maintenance::self_update(force),
        Commands::State(cmd) => state::run(cmd),
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
            commands::serve::wake(&pm::state::serve_files::ServeFiles::global()?);
            commands::tmux_push::push(&paths::global_projects_dir()?, server)
        }
        Commands::Tmux(TmuxCommands::Watch) => {
            commands::tmux_watch::watch(&paths::global_projects_dir()?, server)
        }
        Commands::Workflow(cmd) => workflow::run(cmd),
    }
}
