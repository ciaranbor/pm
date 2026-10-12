//! `pm onboard`: set up this machine's home project and start its
//! supervisor with a brief to onboard the user.
//!
//! Home is an ordinary registered project at `$XDG_DATA_HOME/pm/home`,
//! whose empty `main/` repo belongs to no project's code. It shadows the
//! `orchestrator` workflow with a team of [`SUPERVISOR`], so its main scope
//! runs the supervisor, and everything that follows main's orchestrator
//! ([`scope::orchestrator_definition`]) follows it. The global config's
//! `home` key names it on this machine; only the snapshot's `home` flag and
//! this command read it.
//!
//! Every step is idempotent, so a rerun finishes a setup cut short and
//! otherwise only opens home. The shadow is written before anything is
//! spawned in home, since `pm init`'s spawn would launch bundled `main`.

use std::path::{Path, PathBuf};

use crate::commands::config::{self, Files, Tier};
use crate::commands::skills::{self, GlobalStore};
use crate::commands::{feat_common, harness_check, hooks_install, init};
use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::messages;
use crate::state::agent::AgentRegistry;
use crate::state::dirs::Dirs;
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectEntry};
use crate::state::scope;

/// The home project's registered name.
pub const HOME_NAME: &str = "home";

/// The definition home's main scope runs.
pub const SUPERVISOR: &str = "supervisor";

/// The supervisor's first message, queued as a `feat new` brief is: from
/// pm, with no one to reply to.
pub const BRIEF: &str = "The user just ran `pm onboard` on this machine. Onboard them: ask \
what they want set up, then walk them through it one step at a time.";

const SHADOW_DESCRIPTION: &str =
    "This machine's home: the supervisor, working for the user across every project";

const SHADOW_WORKFLOW: &str = "# orchestrator\n\nThis machine's home project. Its main \
scope runs the supervisor, which works for the user across every project.\n\n## any agent\n\n\
Report to the user in your own session.\n";

/// Where home lives under `dirs`.
pub fn home_root(dirs: &Dirs) -> PathBuf {
    dirs.data_home.join("pm").join(HOME_NAME)
}

/// Whether the project `name` at `root` is this machine's home: the one the
/// global config's `home` names, where `pm onboard` puts it. A project that
/// took the name after home was deleted is not.
pub fn is_home(config: &GlobalConfig, name: &str, root: &Path) -> bool {
    config.home.as_deref() == Some(name)
        && paths::global_dirs().is_ok_and(|dirs| root == home_root(&dirs))
}

/// Home's root, when it is registered and on this machine. A registered
/// home that isn't here, but whose state a remote holds, is an error: `pm
/// restore` brings it back with that state, where a new one would not. So
/// is anything at `root` when home is not here, which a new home can't be
/// created over.
pub fn find(projects_dir: &Path, root: &Path) -> Result<Option<PathBuf>> {
    match ProjectEntry::load(projects_dir, HOME_NAME) {
        Ok(entry) if entry.presence().is_here() => {
            let registered = entry.root_path();
            if registered != root {
                return Err(PmError::Onboard(format!(
                    "a project named '{HOME_NAME}' is already registered at {}; home needs \
                     that name, at {}",
                    registered.display(),
                    root.display()
                )));
            }
            return Ok(Some(registered));
        }
        Ok(entry) if entry.state_remote.is_some() => {
            return Err(PmError::Onboard(format!(
                "home is registered but not on this machine; `pm restore --project \
                 {HOME_NAME}` brings it back with its state, then run `pm onboard` again"
            )));
        }
        Ok(_) | Err(PmError::ProjectNotFound(_)) => {}
        Err(e) => return Err(e),
    }
    if std::fs::symlink_metadata(root).is_ok() {
        return Err(PmError::Onboard(format!(
            "{} is in the way of a new home (a deleted home leaves it behind); remove it, then \
             run `pm onboard` again",
            root.display()
        )));
    }
    Ok(None)
}

/// Install what `pm init` would — pm's hooks and the global asset tier —
/// so [`readiness`] judges the harnesses as home's spawn will find them.
pub fn install(global: &GlobalStore) -> Result<()> {
    hooks_install::install_in(&global.home, false)?;
    skills::install_global_in(global)?;
    Ok(())
}

/// What stands between the supervisor and a working session on one harness.
pub struct Readiness {
    pub harness: Harness,
    /// Empty when it is ready.
    pub problems: Vec<String>,
}

/// Each supported harness's [`Readiness`] to run the supervisor, judged as
/// a spawn would under `global` plus a supervisor row naming it.
pub fn readiness(global: &GlobalConfig, root: &Path, user_home: &Path) -> Result<Vec<Readiness>> {
    let worktree = paths::main_worktree(root);
    Harness::SUPPORTED
        .iter()
        .map(|&harness| {
            let mut problems =
                harness_check::problems_on(harness, SUPERVISOR, global, &worktree, user_home)?;
            if problems.is_empty() && harness.logged_in() == Some(false) {
                problems.push(not_logged_in(harness, global));
            }
            Ok(Readiness { harness, problems })
        })
        .collect()
}

fn not_logged_in(harness: Harness, global: &GlobalConfig) -> String {
    let mut problem = format!(
        "not logged in: {}",
        harness.credentials_step(&global.harness)
    );
    if cfg!(target_os = "macos") && harness.reads_keychain() {
        problem.push_str(&format!(
            "; if you are, unlock the login keychain (`{}`)",
            crate::keychain::UNLOCK
        ));
    }
    problem
}

/// The harness to offer first: `configured` when it is ready, else the
/// first ready one.
pub fn default_choice(statuses: &[Readiness], configured: Option<Harness>) -> Option<Harness> {
    let ready = |h: &Harness| {
        statuses
            .iter()
            .any(|s| s.harness == *h && s.problems.is_empty())
    };
    configured.filter(ready).or_else(|| {
        statuses
            .iter()
            .find(|s| s.problems.is_empty())
            .map(|s| s.harness)
    })
}

/// Record `harness` as the supervisor's, globally; returns what to print.
pub fn set_harness(config_dir: &Path, harness: Harness) -> Result<Vec<String>> {
    config::set(
        &global_files(config_dir),
        Tier::Global,
        &format!("agents.harness.{SUPERVISOR}"),
        &harness.to_string(),
    )
}

/// Create home at `root` without spawning anything in it.
pub fn create(
    root: &Path,
    projects_dir: &Path,
    global: &GlobalStore,
    tmux_server: Option<&str>,
) -> Result<()> {
    init::init_in(
        root,
        Some(HOME_NAME),
        projects_dir,
        global,
        None,
        tmux_server,
    )?;
    Ok(())
}

/// Shadow home's `orchestrator` workflow with the supervisor's team, unless
/// a shadow is already there.
pub fn write_shadow(root: &Path) -> Result<()> {
    let dir = paths::workflows_dir(root).join(scope::MAIN_WORKFLOW);
    if dir.join("config.toml").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("workflow.md"), SHADOW_WORKFLOW)?;
    std::fs::write(
        dir.join("config.toml"),
        format!("description = \"{SHADOW_DESCRIPTION}\"\nagents = [\"{SUPERVISOR}\"]\n"),
    )?;
    Ok(())
}

/// Name home in the global config, unless it already does.
pub fn mark(config_dir: &Path) -> Result<()> {
    if GlobalConfig::load(config_dir)?.home.as_deref() == Some(HOME_NAME) {
        return Ok(());
    }
    config::set(&global_files(config_dir), Tier::Global, "home", HOME_NAME)?;
    Ok(())
}

/// Start home's supervisor unless it is running: a first spawn gets
/// [`BRIEF`], and a stopped one resumes its conversation. Returns what to
/// print, `None` when it was already running.
pub fn spawn_supervisor(root: &Path, tmux_server: Option<&str>) -> Result<Option<String>> {
    let registry = AgentRegistry::load(&paths::agents_dir(root), scope::MAIN)?;
    let name = scope::orchestrator(root);
    match registry.get(&name) {
        Some(entry) if entry.active => return Ok(None),
        Some(_) => {}
        None => queue_brief(root, name)?,
    }
    init::spawn_main(root, tmux_server).map(Some)
}

/// Queue [`BRIEF`] for `name` unless its inbox already holds a message: a
/// spawn that failed leaves the brief queued and nothing registered, so a
/// rerun reaches here again.
fn queue_brief(root: &Path, name: String) -> Result<()> {
    if messages::unread_count(&paths::messages_dir(root), scope::MAIN, &name) == 0 {
        feat_common::enqueue_initial_context(root, scope::MAIN, &[name], BRIEF)?;
    }
    Ok(())
}

fn global_files(config_dir: &Path) -> Files {
    Files {
        project_root: None,
        global_dir: config_dir.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn status(harness: Harness, problems: &[&str]) -> Readiness {
        Readiness {
            harness,
            problems: problems.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn default_choice_keeps_a_ready_configured_harness_else_takes_the_first_ready() {
        let statuses = [
            status(Harness::ClaudeCode, &["not logged in"]),
            status(Harness::Codex, &[]),
            status(Harness::OpenCode, &[]),
        ];
        assert_eq!(default_choice(&statuses, None), Some(Harness::Codex));
        assert_eq!(
            default_choice(&statuses, Some(Harness::OpenCode)),
            Some(Harness::OpenCode)
        );
        assert_eq!(
            default_choice(&statuses, Some(Harness::ClaudeCode)),
            Some(Harness::Codex)
        );
        assert_eq!(
            default_choice(&[status(Harness::Codex, &["x"])], None),
            None
        );
    }

    #[test]
    fn not_logged_in_says_to_unlock_the_keychain_only_for_a_keychain_reader_on_macos() {
        let global = GlobalConfig::default();
        let says_unlock = |h| not_logged_in(h, &global).contains(crate::keychain::UNLOCK);
        assert_eq!(says_unlock(Harness::ClaudeCode), cfg!(target_os = "macos"));
        assert!(!says_unlock(Harness::OpenCode));
    }

    #[test]
    fn a_rerun_after_a_failed_spawn_queues_no_second_brief() {
        let dir = tempdir().unwrap();
        queue_brief(dir.path(), SUPERVISOR.into()).unwrap();
        queue_brief(dir.path(), SUPERVISOR.into()).unwrap();
        let messages = paths::messages_dir(dir.path());
        assert_eq!(
            messages::unread_count(&messages, scope::MAIN, SUPERVISOR),
            1
        );
    }

    #[test]
    fn only_the_named_project_at_homes_root_is_home() {
        let config = GlobalConfig {
            home: Some(HOME_NAME.into()),
            ..Default::default()
        };
        let root = home_root(&paths::global_dirs().unwrap());
        assert!(is_home(&config, HOME_NAME, &root));
        assert!(!is_home(&config, HOME_NAME, Path::new("/elsewhere/home")));
        assert!(!is_home(&config, "app", &root));
        assert!(!is_home(&GlobalConfig::default(), HOME_NAME, &root));
    }

    #[test]
    fn find_refuses_what_a_new_home_cannot_replace() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let root = dir.path().join("data/pm/home");
        assert_eq!(find(&projects_dir, &root).unwrap(), None);
        std::fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        assert!(matches!(
            find(&projects_dir, &root),
            Err(PmError::Onboard(m)) if m.contains("in the way")
        ));
        std::fs::remove_dir_all(&root).unwrap();

        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(paths::main_worktree(&elsewhere)).unwrap();
        let mut entry = ProjectEntry {
            root: elsewhere.display().to_string(),
            main_branch: "main".into(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, HOME_NAME).unwrap();
        assert!(matches!(
            find(&projects_dir, &root),
            Err(PmError::Onboard(m)) if m.contains("already registered")
        ));

        entry.root = root.display().to_string();
        entry.save(&projects_dir, HOME_NAME).unwrap();
        assert_eq!(find(&projects_dir, &root).unwrap(), None);
        entry.state_remote = Some("git@host:home-state.git".into());
        entry.save(&projects_dir, HOME_NAME).unwrap();
        assert!(matches!(
            find(&projects_dir, &root),
            Err(PmError::Onboard(m)) if m.contains("pm restore --project home")
        ));

        std::fs::create_dir_all(paths::main_worktree(&root)).unwrap();
        assert_eq!(find(&projects_dir, &root).unwrap(), Some(root));
    }

    #[test]
    fn home_runs_the_supervisor_as_its_orchestrator_with_the_brief() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let root = dir.path().join("pm/home");
        // Scoped, so the test server's cleanup finds its session.
        let name = server.scope(HOME_NAME);
        init::init_in(
            &root,
            Some(&name),
            &dir.path().join("projects"),
            &GlobalStore::resolve().unwrap(),
            None,
            server.name(),
        )
        .unwrap();
        write_shadow(&root).unwrap();
        write_shadow(&root).unwrap();
        assert_eq!(scope::orchestrator_definition(&root), SUPERVISOR);

        spawn_supervisor(&root, server.name()).unwrap().unwrap();
        let registry = AgentRegistry::load(&paths::agents_dir(&root), scope::MAIN).unwrap();
        assert!(registry.get(SUPERVISOR).is_some_and(|e| e.active));
        assert!(registry.get(scope::DEFAULT_ORCHESTRATOR).is_none());
        let messages = paths::messages_dir(&root);
        let briefs = messages::check(&messages, scope::MAIN, SUPERVISOR).unwrap();
        assert_eq!(briefs.len(), 1);
        assert!(messages::is_no_reply(&briefs[0].sender));
        assert_eq!(spawn_supervisor(&root, server.name()).unwrap(), None);
    }
}
