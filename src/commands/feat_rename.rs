//! `pm feat rename`: the branch, worktree, tmux session and every piece of
//! state keyed by the feature's name — the `base` of features stacked on it
//! included — move together or not at all; the
//! sessions recorded at the old worktree path are then carried to the new
//! one, which can no longer undo the rename and so only reports.
//!
//! An agent running through the rename was started at the old path, waits
//! on the old scope's inbox and holds a session recorded there, so it is
//! stopped before the sessions are carried and respawned, resumed, after.
//! The one exception is the window the rename runs from: killing it ends
//! the rename, so it is killed last, after the respawns, and its session is
//! carried as it stands while it still runs.
//!
//! Stopping an agent mid-turn loses its in-flight tool call, so the rename
//! refuses, before changing anything, while any other agent is busy
//! ([`super::running_agents`]); a dead agent has no turn to lose, so it is
//! respawned like an idle one. `--force` stops busy agents anyway and
//! queues each a message to resume, from a sender it cannot reply to; the
//! agent the rename runs from, mid-turn by definition, always gets one. An
//! agent idle at the check can still start a turn before it is stopped;
//! that window is accepted.
//!
//! Every inbox in the project is rewritten in place for the new scope, not
//! re-sent: unread messages from the old scope, since a reply goes to a
//! message's scope and a stale copy of `main`'s ready message would name a
//! summary path that no longer exists, and `.last_read`, so that `pm msg
//! reply` follows the rename.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::messages;
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

use super::agent_restart::restarted_line;
use super::agent_spawn::agent_spawn;
use super::feat_status::ready_body;
use super::harness_migrate::{Carry, carry_sessions};
use super::running_agents::{RunningAgent, busy_in_scope, running_in_scope, runs_this_process};

const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The sender of the message that tells an interrupted agent to resume;
/// no agent has this name, so it cannot be replied to.
const RESUME_SENDER: &str = "no-reply-rename";

/// What an agent stopped mid-turn is told: `caller` for the one that ran
/// the rename, whose last tool call is the rename itself.
fn resume_body(old_name: &str, new_name: &str, new_worktree: &Path, caller: bool) -> String {
    let last_call = if caller {
        "That rename completed."
    } else {
        "Your last tool call may not have completed: check its effect."
    };
    format!(
        "`pm feat rename {old_name} {new_name}` restarted you mid-turn; your worktree is now \
         {}. {last_call} Then resume the task you were working on.",
        new_worktree.display()
    )
}

/// State keyed by the feature's name, as `(old, new)` paths.
fn scoped_state(project_root: &Path, old_name: &str, new_name: &str) -> [(PathBuf, PathBuf); 3] {
    let agents = paths::agents_dir(project_root);
    let messages = paths::messages_dir(project_root);
    [
        (
            agents.join(format!("{old_name}.toml")),
            agents.join(format!("{new_name}.toml")),
        ),
        (messages.join(old_name), messages.join(new_name)),
        (
            paths::summary_path(project_root, old_name),
            paths::summary_path(project_root, new_name),
        ),
    ]
}

/// A completed rename.
#[derive(Debug)]
pub struct Renamed {
    /// What the user needs to know about the agents' sessions.
    pub report: Vec<String>,
    /// The agent window the rename ran from, left to [`Renamed::finish`].
    caller_window: Option<String>,
}

impl Renamed {
    /// Kill the agent window the rename ran from, if it ran from one. That
    /// ends the calling process, so it comes after the report is printed.
    pub fn finish(self, tmux_server: Option<&str>) {
        if let Some(window) = self.caller_window {
            let _ = tmux::kill_window(tmux_server, &window);
        }
    }
}

/// Rename a feature. `force` stops agents that are mid-turn instead of
/// refusing.
pub fn feat_rename(
    project_root: &Path,
    old_name: &str,
    new_name: &str,
    force: bool,
    tmux_server: Option<&str>,
) -> Result<Renamed> {
    feat_rename_in(project_root, old_name, new_name, force, tmux_server, None)
}

/// [`feat_rename`] with the home holding the session stores.
pub fn feat_rename_in(
    project_root: &Path,
    old_name: &str,
    new_name: &str,
    force: bool,
    tmux_server: Option<&str>,
    home: Option<&Path>,
) -> Result<Renamed> {
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Validate: old feature must exist
    let state = FeatureState::load(&features_dir, old_name)?;

    // Validate: new name must not already exist as a feature
    if FeatureState::exists(&features_dir, new_name) {
        return Err(PmError::FeatureAlreadyExists(new_name.to_string()));
    }

    // Validate: new name must not already exist as a branch
    let main_repo = paths::main_worktree(project_root);
    if git::branch_exists(&main_repo, new_name)? {
        return Err(PmError::SafetyCheck(format!(
            "branch '{new_name}' already exists"
        )));
    }

    super::feat_summary::ensure_no_untriaged(project_root, new_name)?;
    let scoped = scoped_state(project_root, old_name, new_name);
    if let Some((_, taken)) = scoped.iter().find(|(_, new)| new.exists()) {
        return Err(PmError::SafetyCheck(format!(
            "{} is left over from an earlier feature '{new_name}'; remove it first",
            taken.display()
        )));
    }

    let stacked: Vec<(String, FeatureState)> = FeatureState::list(&features_dir)?
        .into_iter()
        .filter(|(name, child)| name != old_name && child.base == state.branch)
        .collect();

    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    let old_worktree_path = project_root.join(&state.worktree);
    let new_worktree_path = project_root.join(new_name);

    let busy = busy_in_scope(project_root, project_name, old_name, tmux_server);
    if !busy.is_empty() && !force {
        return Err(PmError::SafetyCheck(format!(
            "agent(s) {} are mid-turn, and the rename restarts them; \
             wait until they are idle, or pass --force to interrupt them and have them resume",
            busy.iter()
                .map(|a| format!("'{a}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    // Step 1: Rename git branch
    git::rename_branch(&main_repo, &state.branch, new_name)?;

    // Step 2: Move git worktree
    if old_worktree_path.exists()
        && let Err(e) = git::move_worktree(&main_repo, &old_worktree_path, &new_worktree_path)
    {
        // Rollback branch rename
        let _ = git::rename_branch(&main_repo, new_name, &state.branch);
        return Err(e);
    }

    // Step 3: Rename tmux session
    let old_session = tmux::session_name(project_name, old_name);
    let new_session = tmux::session_name(project_name, new_name);
    if tmux::has_session(tmux_server, &old_session)?
        && let Err(e) = tmux::rename_session(tmux_server, &old_session, &new_session)
    {
        // Rollback worktree move and branch rename
        let _ = git::move_worktree(&main_repo, &new_worktree_path, &old_worktree_path);
        let _ = git::rename_branch(&main_repo, new_name, &state.branch);
        return Err(e);
    }

    // Step 4: Move the state keyed by name, then the state file (save new,
    // delete old).
    // Capture the original branch name before the struct-update moves the
    // rest of `state` out — we still need it for the rollback path.
    let original_branch = state.branch.clone();
    let updated = FeatureState {
        branch: new_name.to_string(),
        worktree: new_name.to_string(),
        ..state
    };
    let mut moved = Vec::new();
    let mut rebased = Vec::new();
    let saved = scoped
        .iter()
        .filter(|(old, _)| old.exists())
        .try_for_each(|(old, new)| {
            std::fs::rename(old, new)?;
            moved.push((old, new));
            Ok(())
        })
        .and_then(|()| {
            stacked.iter().try_for_each(|(name, child)| {
                let child = FeatureState {
                    base: new_name.to_string(),
                    ..child.clone()
                };
                child.save(&features_dir, name)?;
                rebased.push(name);
                Ok(())
            })
        })
        .and_then(|()| updated.save(&features_dir, new_name));
    if let Err(e) = saved {
        // Rollback everything. Use `original_branch` (the original branch
        // name) — NOT `old_name` (the feature name). They can differ for
        // adopted features whose branch name was preserved via
        // --name-override (e.g. branch="ciaran/eval", feature="eval").
        for (name, child) in &stacked {
            if rebased.contains(&name) {
                let _ = child.save(&features_dir, name);
            }
        }
        for (old, new) in moved {
            let _ = std::fs::rename(new, old);
        }
        let _ = tmux::rename_session(tmux_server, &new_session, &old_session);
        let _ = git::move_worktree(&main_repo, &new_worktree_path, &old_worktree_path);
        let _ = git::rename_branch(&main_repo, new_name, &original_branch);
        return Err(e);
    }
    // Only delete old state after new one is safely written
    let _ = FeatureState::delete(&features_dir, old_name);
    let mut report = rescope_inboxes(project_root, old_name, new_name);

    if !new_worktree_path.exists() {
        let _ = crate::state::runtime::remove_scope(project_root, old_name);
        return Ok(Renamed {
            report,
            caller_window: None,
        });
    }
    // A harness still writing its transcript while it is copied would be
    // resumed without what it wrote after.
    let running = running_in_scope(project_root, project_name, new_name, tmux_server);
    let mut stopped = Vec::new();
    let mut own_window = None;
    for agent in &running {
        let processes = tmux::pane_processes(tmux_server, &agent.window).unwrap_or_default();
        if runs_this_process(&processes) {
            let _ = tmux::rename_window(
                tmux_server,
                &agent.window,
                &format!("{}-renaming", agent.entry.window_name),
            );
            own_window = Some(agent);
            continue;
        }
        let _ = tmux::kill_window(tmux_server, &agent.window);
        stopped.push((&agent.name, processes));
    }
    let all: Vec<tmux::Process> = stopped.iter().flat_map(|(_, ps)| ps.clone()).collect();
    let left = tmux::wait_for_exit(&all, EXIT_WAIT);
    let _ = crate::state::runtime::remove_scope(project_root, old_name);
    let lingering: Vec<String> = stopped
        .iter()
        .filter(|(_, ps)| ps.iter().any(|p| left.contains(p)))
        .map(|(agent, _)| {
            format!(
                "Warning: agent '{agent}' had not exited after {}s; its session was carried \
                 as it stood then",
                EXIT_WAIT.as_secs()
            )
        })
        .collect();
    report.extend(carry_sessions(&Carry {
        from: &old_worktree_path,
        to: &new_worktree_path,
        project_root,
        home,
        tmux_server,
    }));
    report.extend(lingering);
    let messages_dir = paths::messages_dir(project_root);
    for RunningAgent { name: agent, .. } in &running {
        let caller = own_window.is_some_and(|own| own.name == *agent);
        if caller || busy.contains(agent) {
            let sent = messages::send(
                &messages_dir,
                new_name,
                agent,
                RESUME_SENDER,
                &resume_body(old_name, new_name, &new_worktree_path, caller),
            );
            match sent {
                Ok(_) if caller => {}
                Ok(_) => report.push(format!(
                    "Interrupted agent '{agent}' mid-turn; it is told to resume"
                )),
                Err(e) => report.push(format!(
                    "Warning: agent '{agent}' was interrupted mid-turn and could not be told \
                     to resume: {e}"
                )),
            }
        }
        report.push(
            match agent_spawn(project_root, new_name, agent, None, None, tmux_server) {
                Ok((outcome, _, notes)) => restarted_line(agent, outcome, &notes),
                Err(e) => format!(
                    "Warning: agent '{agent}' was stopped and could not be restarted: {e}; \
                     run `pm agent spawn {agent}` in {}",
                    new_worktree_path.display()
                ),
            },
        );
    }
    let caller_window = own_window.map(|own| {
        if let Ok(Some(new_window)) =
            tmux::find_window(tmux_server, &new_session, &own.entry.window_name)
        {
            let _ = tmux::select_window(tmux_server, &new_window);
        }
        own.window.clone()
    });
    Ok(Renamed {
        report,
        caller_window,
    })
}

/// Runs after the rename is committed, so a failure is only reported.
fn rescope_inboxes(project_root: &Path, old_name: &str, new_name: &str) -> Vec<String> {
    let body = |b: &str| {
        [true, false]
            .into_iter()
            .find(|&repliable| b == ready_body(project_root, old_name, repliable))
            .map_or_else(
                || b.to_string(),
                |repliable| ready_body(project_root, new_name, repliable),
            )
    };
    let messages_dir = paths::messages_dir(project_root);
    let inboxes = match messages::inboxes(&messages_dir) {
        Ok(inboxes) => inboxes,
        Err(e) => {
            return vec![format!(
                "Warning: messages from '{old_name}' still name it: {e}"
            )];
        }
    };
    inboxes
        .iter()
        .filter_map(|(scope, agent)| {
            messages::rescope(&messages_dir, scope, agent, old_name, new_name, body)
                .err()
                .map(|e| {
                    format!(
                        "Warning: messages from '{old_name}' in {scope}/{agent}'s inbox still \
                         name it: {e}"
                    )
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::running_agents::is_idle;
    use crate::commands::{feat_new, init};
    use crate::state::agent::AgentRegistry;
    use crate::state::feature::Progress;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn rename_updates_state_file() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert!(!FeatureState::exists(&features_dir, "login"));
        assert!(FeatureState::exists(&features_dir, "auth"));

        let state = FeatureState::load(&features_dir, "auth").unwrap();
        assert_eq!(state.branch, "auth");
        assert_eq!(state.worktree, "auth");
    }

    #[test]
    fn rename_updates_git_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        let main_repo = paths::main_worktree(&project_path);
        assert!(!git::branch_exists(&main_repo, "login").unwrap());
        assert!(git::branch_exists(&main_repo, "auth").unwrap());
    }

    #[test]
    fn rename_moves_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        assert!(!project_path.join("login").exists());
        assert!(project_path.join("auth").exists());
        assert!(project_path.join("auth").is_dir());
    }

    #[test]
    fn rename_updates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "auth")).unwrap()
        );
    }

    #[test]
    fn rename_preserves_state_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        let features_dir = paths::features_dir(&project_path);
        let original = FeatureState::load(&features_dir, "login").unwrap();

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        let renamed = FeatureState::load(&features_dir, "auth").unwrap();
        assert_eq!(renamed.status, original.status);
        assert_eq!(renamed.context, original.context);
        assert_eq!(renamed.created, original.created);
        assert_eq!(renamed.pr, original.pr);
        assert_eq!(renamed.base, original.base);
    }

    #[test]
    fn rename_of_a_ready_feature_carries_its_summary_and_ready_message() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project_path, "login").unwrap(),
            "notes",
        )
        .unwrap();
        crate::commands::feat_status::feat_status(
            &project_path,
            "login",
            Progress::Ready,
            None,
            Some("implementer"),
        )
        .unwrap();

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        let features_dir = paths::features_dir(&project_path);
        assert_eq!(
            FeatureState::load(&features_dir, "auth").unwrap().progress,
            Progress::Ready
        );
        assert!(!paths::summary_path(&project_path, "login").exists());
        let messages_dir = paths::messages_dir(&project_path);
        let msg = messages::read_at(&messages_dir, "main", "main", "implementer", 1)
            .unwrap()
            .unwrap();
        assert_eq!(msg.meta.sender_scope.as_deref(), Some("auth"));
        assert_eq!(msg.body, ready_body(&project_path, "auth", true));
        assert_eq!(
            std::fs::read_to_string(paths::summary_path(&project_path, "auth")).unwrap(),
            "notes"
        );
    }

    #[test]
    fn rename_nonexistent_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let result = feat_rename(
            &project_path,
            "nonexistent",
            "new-name",
            false,
            server.name(),
        );
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn rename_to_existing_feature_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &TestServer::registry_dir(&project_path),
            "signup",
            server.name(),
        ))
        .unwrap();

        let result = feat_rename(&project_path, "login", "signup", false, server.name());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));

        // Original feature should be untouched
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
    }

    #[test]
    fn rename_to_existing_branch_fails() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");

        // Create a branch without a feature
        let main_repo = paths::main_worktree(&project_path);
        git::create_branch(&main_repo, "taken-branch").unwrap();

        let result = feat_rename(&project_path, "login", "taken-branch", false, server.name());
        assert!(result.is_err());

        // Original feature should be untouched
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(git::branch_exists(&main_repo, "login").unwrap());
    }

    fn use_opencode(project_path: &Path, binary: String) {
        let pm_dir = paths::pm_dir(project_path);
        let mut config = ProjectConfig::load(&pm_dir).unwrap();
        config
            .agents
            .harness
            .insert("qa".to_string(), "opencode".to_string());
        config.harness.opencode.binary = Some(binary);
        config.save(&pm_dir).unwrap();
    }

    fn claude_sessions(home: &Path, worktree: &Path) -> std::path::PathBuf {
        home.join(".claude/projects")
            .join(crate::testing::claude_key(worktree))
    }

    #[test]
    fn rename_keeps_the_agents_and_their_messages() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project_path, &session, "login", "reviewer");
        let messages_dir = paths::messages_dir(&project_path);
        crate::messages::send(&messages_dir, "login", "reviewer", "user", "look at this").unwrap();

        feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap();

        let agents_dir = paths::agents_dir(&project_path);
        assert!(
            AgentRegistry::load(&agents_dir, "auth")
                .unwrap()
                .get("reviewer")
                .is_some()
        );
        assert!(
            AgentRegistry::load(&agents_dir, "login")
                .unwrap()
                .agents
                .is_empty()
        );
        let unread = crate::messages::check(&messages_dir, "auth", "reviewer").unwrap();
        assert_eq!(unread.len(), 1);
        assert!(!messages_dir.join("login").exists());
    }

    #[test]
    fn rename_keeps_stacked_features_on_the_renamed_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let features_dir = paths::features_dir(&project_path);
        let registry = TestServer::registry_dir(&project_path);
        for name in ["child", "sibling"] {
            feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
                &project_path,
                &registry,
                name,
                server.name(),
            ))
            .unwrap();
        }
        let mut child = FeatureState::load(&features_dir, "child").unwrap();
        child.base = "login".to_string();
        child.save(&features_dir, "child").unwrap();
        let sibling_base = FeatureState::load(&features_dir, "sibling").unwrap().base;

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        let child = FeatureState::load(&features_dir, "child").unwrap();
        assert_eq!(child.base, "auth");
        let main_branch = crate::state::project::ProjectEntry::load(
            &registry,
            &ProjectConfig::load(&paths::pm_dir(&project_path))
                .unwrap()
                .project
                .name,
        )
        .unwrap()
        .main_branch;
        assert_eq!(
            crate::state::feature::base_checkout(&project_path, &main_branch, &child.base)
                .unwrap()
                .scope,
            "auth"
        );
        assert_eq!(
            FeatureState::load(&features_dir, "sibling").unwrap().base,
            sibling_base
        );
    }

    #[test]
    fn rename_refuses_a_name_that_has_state_left_over() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let leftover = paths::messages_dir(&project_path).join("auth/reviewer");
        std::fs::create_dir_all(&leftover).unwrap();

        let err = feat_rename(&project_path, "login", "auth", false, server.name())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("left over from an earlier feature 'auth'"),
            "{err}"
        );
        assert!(leftover.exists());
        assert!(project_path.join("login").exists());
        assert!(git::branch_exists(&paths::main_worktree(&project_path), "login").unwrap());
    }

    #[test]
    fn rename_restarts_a_running_agent_on_its_carried_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let project_path = project_path.canonicalize().unwrap();
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project_path, &session, "login", "reviewer");
        let agents_dir = paths::agents_dir(&project_path);
        let mut registry = AgentRegistry::load(&agents_dir, "login").unwrap();
        registry.get_mut("reviewer").unwrap().session_id = "running".to_string();
        registry.save(&agents_dir, "login").unwrap();

        let old = project_path.join("login");
        let new = project_path.join("auth");
        let store = claude_sessions(dir.path(), &old);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("running.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", old.display()),
        )
        .unwrap();

        let report = feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap()
        .report;

        assert_eq!(
            std::fs::read_to_string(claude_sessions(dir.path(), &new).join("running.jsonl"))
                .unwrap(),
            format!("{{\"cwd\":\"{}\"}}\n", new.display())
        );
        assert_eq!(
            report,
            [
                format!(
                    "Copied 1 Claude session(s) from {} to {}",
                    old.display(),
                    new.display()
                ),
                "Restarted agent 'reviewer' (resumed session)".to_string(),
            ]
        );
        // The window is a new one: the fake agent's was waiting in the hook.
        let window = tmux::find_window(
            server.name(),
            &tmux::session_name(&project_name, "auth"),
            "reviewer",
        )
        .unwrap()
        .unwrap();
        assert!(!is_idle(
            &tmux::pane_processes(server.name(), &window).unwrap()
        ));
        let registry = AgentRegistry::load(&agents_dir, "auth").unwrap();
        assert!(registry.get("reviewer").unwrap().active);
        let messages_dir = paths::messages_dir(&project_path);
        assert!(
            messages::check(&messages_dir, "auth", "reviewer")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rename_refuses_while_an_agent_is_mid_turn_unless_forced() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_fake_agent(&project_path, &session, "login", "reviewer");

        let err = feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("'reviewer' are mid-turn"), "{err}");
        assert!(err.contains("--force"), "{err}");
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(project_path.join("login").exists());
        assert!(tmux::has_session(server.name(), &session).unwrap());
        assert!(FeatureState::exists(
            &paths::features_dir(&project_path),
            "login"
        ));

        let report = feat_rename_in(
            &project_path,
            "login",
            "auth",
            true,
            server.name(),
            Some(dir.path()),
        )
        .unwrap()
        .report;

        assert!(
            report.contains(
                &"Interrupted agent 'reviewer' mid-turn; it is told to resume".to_string()
            ),
            "{report:?}"
        );
        let messages_dir = paths::messages_dir(&project_path);
        let msg = messages::read_at(&messages_dir, "auth", "reviewer", RESUME_SENDER, 1)
            .unwrap()
            .unwrap();
        assert!(
            msg.body
                .contains(&project_path.join("auth").display().to_string()),
            "{}",
            msg.body
        );
        assert_eq!(msg.meta.sender_scope, None);
    }

    #[test]
    fn rename_restarts_an_agent_whose_harness_exited_without_force() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_dead_fake_agent(&project_path, &session, "login", "reviewer");

        let report = feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap()
        .report;

        assert!(
            report
                .iter()
                .any(|l| l.starts_with("Restarted agent 'reviewer'")),
            "{report:?}"
        );
        assert!(
            messages::check(&paths::messages_dir(&project_path), "auth", "reviewer")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rename_refuses_a_name_whose_summary_waits_for_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let summary = paths::summary_path(&project_path, "auth");
        std::fs::create_dir_all(summary.parent().unwrap()).unwrap();
        std::fs::write(&summary, "earlier").unwrap();

        let err = feat_rename(&project_path, "login", "auth", false, server.name()).unwrap_err();

        assert!(matches!(err, PmError::Summary(_)), "{err}");
        assert!(git::branch_exists(&paths::main_worktree(&project_path), "login").unwrap());
        assert_eq!(std::fs::read_to_string(&summary).unwrap(), "earlier");
    }

    #[test]
    fn a_reply_from_main_after_a_rename_reaches_the_renamed_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project_path, &session, "login", "dev");
        let messages_dir = paths::messages_dir(&project_path);
        messages::send_with_scope(&messages_dir, "main", "main", "dev", "done", Some("login"))
            .unwrap();
        crate::commands::agent_read::agent_read(&project_path, "main", "main", None, None).unwrap();

        feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap();
        crate::commands::msg_reply::msg_reply(
            &project_path,
            "main",
            "main",
            "thanks",
            server.name(),
        )
        .unwrap();

        let unread = messages::check(&messages_dir, "auth", "dev").unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].sender, "main");
    }

    #[test]
    fn rename_readdresses_other_features_inboxes() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let messages_dir = paths::messages_dir(&project_path);
        let send = |body| {
            messages::send_with_scope(&messages_dir, "signup", "qa", "dev", body, Some("login"))
                .unwrap()
        };
        send("read");
        crate::commands::agent_read::agent_read(&project_path, "signup", "qa", None, None).unwrap();
        send("unread");

        feat_rename(&project_path, "login", "auth", false, server.name()).unwrap();

        let scope_of = |index| {
            messages::read_at(&messages_dir, "signup", "qa", "dev", index)
                .unwrap()
                .unwrap()
                .meta
                .sender_scope
        };
        assert_eq!(scope_of(1).as_deref(), Some("login"));
        assert_eq!(scope_of(2).as_deref(), Some("auth"));
        assert_eq!(
            messages::load_last_read(&messages_dir, "signup", "qa")
                .unwrap()
                .unwrap()
                .sender_scope
                .as_deref(),
            Some("auth")
        );
    }

    #[test]
    fn rename_completes_without_a_harness_that_is_not_installed() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let missing = dir.path().join("no-such-opencode");
        use_opencode(&project_path, missing.to_string_lossy().into_owned());

        let report = feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap()
        .report;

        assert!(FeatureState::exists(
            &paths::features_dir(&project_path),
            "auth"
        ));
        assert!(
            report.contains(&format!(
                "Skipped opencode sessions: `{}` could not be run",
                missing.display()
            )),
            "{report:?}"
        );
    }

    #[test]
    fn rename_completes_and_says_which_sessions_a_failed_move_left_behind() {
        use crate::testing::fake_opencode_sequence;

        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let fake = tempdir().unwrap();
        let listing = r#"{"data":[{"id":"ses_a"}],"cursor":{"previous":null,"next":null}}"#;
        // The server pm starts to move sessions through never comes up.
        use_opencode(
            &project_path,
            fake_opencode_sequence(fake.path(), &["opencode v2.0.18", listing, "", ""], 0),
        );

        let report = feat_rename_in(
            &project_path,
            "login",
            "auth",
            false,
            server.name(),
            Some(dir.path()),
        )
        .unwrap()
        .report;

        assert!(FeatureState::exists(
            &paths::features_dir(&project_path),
            "auth"
        ));
        assert!(project_path.join("auth").exists());
        let old = project_path.join("login");
        let new = project_path.join("auth");
        let report = report.join("\n");
        assert!(
            report.contains(&format!(
                "Warning: opencode sessions of {} were not all carried to {}",
                old.display(),
                new.display()
            )),
            "{report}"
        );
        assert!(
            report.contains(&format!(
                "run `pm harness migrate --harness opencode --from {}` in {}",
                old.display(),
                new.display()
            )),
            "{report}"
        );
    }

    // --- Rollback path tests ---

    #[test]
    fn rename_worktree_move_failure_rolls_back_branch() {
        // Branch rename succeeds, but `git worktree move` fails because the
        // destination path already exists. The rollback must rename the
        // branch back to its original name.
        //
        // Note: `git worktree move login auth` treats an existing directory
        // at `auth` as a target into which `login` should be moved (becomes
        // `auth/login`). To force a real failure we plant a regular file at
        // the destination — git refuses with "fatal: 'auth' already exists".
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        let dest = project_path.join("auth");
        std::fs::write(&dest, "blocker").unwrap();

        let result = feat_rename(&project_path, "login", "auth", false, server.name());
        assert!(result.is_err(), "expected worktree move to fail");

        // Branch rename rolled back: original branch exists, new doesn't.
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());

        // Worktree should still be at its original path.
        assert!(project_path.join("login").exists());

        // State file should still exist under the old name.
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        // Tmux session should still be the original — rename was never tried.
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
    }

    #[test]
    fn rename_tmux_rename_failure_rolls_back_branch_and_worktree() {
        // Branch rename + worktree move succeed, but tmux rename-session
        // fails because a session with the new name already exists. The
        // rollback must restore the worktree path AND the branch name.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        // Pre-create a tmux session with the destination name so
        // `tmux rename-session` refuses.
        tmux::create_session(
            server.name(),
            &tmux::session_name(&project_name, "auth"),
            dir.path(),
        )
        .unwrap();

        let result = feat_rename(&project_path, "login", "auth", false, server.name());
        assert!(result.is_err(), "expected tmux rename to fail");

        // Branch and worktree should be restored to original locations.
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());
        assert!(project_path.join("login").exists());
        assert!(!project_path.join("auth").exists());

        // State file untouched (Step 4 never ran).
        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        // The original session must still be there. The colliding session
        // we pre-created also remains; its presence is what triggered the
        // failure, and rollback shouldn't touch unrelated sessions.
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "auth")).unwrap()
        );
    }

    #[test]
    fn rename_state_save_failure_rolls_back_everything() {
        // Branch + worktree + tmux all succeed, but writing the new state
        // file fails because the atomic-write tmp path is occupied by a
        // directory. The rollback must reverse all three earlier steps.
        //
        // We can't block via `<features_dir>/auth.toml` (a dir there would
        // trip the early `FeatureState::exists("auth")` validation, so the
        // function would fail before reaching the rollback path). Instead
        // we plant the blocker at `.auth.toml.tmp` — the path that
        // `FeatureState::save` writes to before the atomic rename — which
        // makes `std::fs::write` fail with EISDIR but is invisible to
        // `FeatureState::exists`.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, project_name) = server.setup_project_with_feature(dir.path(), "login");

        let features_dir = paths::features_dir(&project_path);
        let blocker = features_dir.join(".auth.toml.tmp");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("blocker"), "x").unwrap();
        let messages_dir = paths::messages_dir(&project_path);
        crate::messages::send(&messages_dir, "login", "reviewer", "user", "hi").unwrap();
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project_path, &session, "login", "reviewer");

        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &TestServer::registry_dir(&project_path),
            "child",
            server.name(),
        ))
        .unwrap();
        let mut child = FeatureState::load(&features_dir, "child").unwrap();
        child.base = "login".to_string();
        child.save(&features_dir, "child").unwrap();

        let result = feat_rename(&project_path, "login", "auth", false, server.name());
        assert!(result.is_err(), "expected state save to fail");

        assert_eq!(
            FeatureState::load(&features_dir, "child").unwrap().base,
            "login"
        );
        assert!(messages_dir.join("login").exists());
        assert!(!messages_dir.join("auth").exists());
        let agents_dir = paths::agents_dir(&project_path);
        assert!(
            AgentRegistry::load(&agents_dir, "login")
                .unwrap()
                .get("reviewer")
                .is_some()
        );
        assert!(!agents_dir.join("auth.toml").exists());

        // Branch back to original name.
        let main_repo = paths::main_worktree(&project_path);
        assert!(git::branch_exists(&main_repo, "login").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());

        // Worktree back to original path.
        assert!(project_path.join("login").exists());
        assert!(!project_path.join("auth").exists());

        // Old state file still exists; the new one was never persisted.
        assert!(FeatureState::exists(&features_dir, "login"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        // Tmux session back to original name.
        assert!(
            tmux::has_session(server.name(), &tmux::session_name(&project_name, "login")).unwrap()
        );
        assert!(
            !tmux::has_session(server.name(), &tmux::session_name(&project_name, "auth")).unwrap()
        );

        // Clean up the blocker so tempdir teardown doesn't trip on the
        // unexpected directory.
        std::fs::remove_dir_all(&blocker).unwrap();
    }

    #[test]
    fn rename_state_save_failure_restores_branch_with_slash() {
        // Regression test for the "old_name vs state.branch" bug: when the
        // adopted feature's branch name differs from its feature name
        // (e.g. branch="ciaran/eval", feature="eval"), a state-save
        // failure during rename must restore the *branch name*, not the
        // feature name.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, projects_dir, _) = server.setup_project(dir.path());

        // Create a slash-bearing branch and adopt it with --name-override
        // so feature_name ("eval") differs from branch ("ciaran/eval").
        let main_repo = paths::main_worktree(&project_path);
        git::create_branch(&main_repo, "ciaran/eval").unwrap();
        crate::commands::feat_adopt::feat_adopt(&crate::commands::feat_adopt::FeatAdoptParams {
            project_root: &project_path,
            projects_dir: &projects_dir,
            name: "ciaran/eval",
            name_override: Some("eval"),
            context: None,
            from: None,
            workflow: None,
            tmux_server: server.name(),
            home: None,
        })
        .unwrap();

        // Confirm the divergence we're testing exists on disk.
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "eval").unwrap();
        assert_eq!(state.branch, "ciaran/eval");
        assert_eq!(state.worktree, "eval");

        // Block the atomic-write tmp path so save fails (see the
        // `rename_state_save_failure_rolls_back_everything` test for why
        // we don't block `auth.toml` directly).
        let blocker = features_dir.join(".auth.toml.tmp");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("blocker"), "x").unwrap();

        let result = feat_rename(&project_path, "eval", "auth", false, server.name());
        assert!(result.is_err(), "expected state save to fail");

        // The original slash-bearing branch must be restored — NOT a flat
        // "eval" branch, which is what the buggy rollback would have
        // produced.
        assert!(
            git::branch_exists(&main_repo, "ciaran/eval").unwrap(),
            "rollback must restore the original branch name, slashes and all"
        );
        assert!(!git::branch_exists(&main_repo, "eval").unwrap());
        assert!(!git::branch_exists(&main_repo, "auth").unwrap());

        // Worktree back at the original feature-name path.
        assert!(project_path.join("eval").exists());
        assert!(!project_path.join("auth").exists());

        // State file still exists under the old feature name; no new one.
        assert!(FeatureState::exists(&features_dir, "eval"));
        assert!(!FeatureState::exists(&features_dir, "auth"));

        std::fs::remove_dir_all(&blocker).unwrap();
    }
}
