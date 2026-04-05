use std::path::Path;

use chrono::Utc;

use crate::error::{PmError, Result};
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::ProjectConfig;
use crate::{git, tmux};

/// Adopt an existing branch as a pm feature: worktree + tmux session + state file.
/// Unlike `feat_new`, this does not create a branch — it must already exist.
///
/// The tmux `server` parameter allows tests to use an isolated tmux server.
/// In production, pass `None` to use the default server.
#[allow(clippy::too_many_arguments)]
pub fn feat_adopt(
    project_root: &Path,
    name: &str,
    name_override: Option<&str>,
    context: Option<&str>,
    from: Option<&Path>,
    no_edit: bool,
    tmux_server: Option<&str>,
    claude_base: Option<&Path>,
) -> Result<String> {
    let branch = name;
    let feature_name = super::feat_new::sanitize_feature_name(branch, name_override)?;
    let features_dir = paths::features_dir(project_root);
    let pm_dir = paths::pm_dir(project_root);

    // Check for duplicate
    if FeatureState::exists(&features_dir, &feature_name) {
        return Err(PmError::FeatureAlreadyExists(feature_name));
    }

    // Verify branch exists
    let main_worktree = project_root.join("main");
    if !git::branch_exists(&main_worktree, branch)? {
        return Err(PmError::BranchNotFound(branch.to_string()));
    }

    // Load project config for name
    let config = ProjectConfig::load(&pm_dir)?;
    let project_name = &config.project.name;

    // Resolve context upfront (file contents or literal text)
    let resolved_context = context.map(super::feat_new::resolve_context).transpose()?;

    // Step 1: Write state with status = initializing
    let now = Utc::now();
    let mut state = FeatureState {
        status: FeatureStatus::Initializing,
        branch: branch.to_string(),
        worktree: feature_name.clone(),
        base: String::new(),
        pr: String::new(),
        context: resolved_context.clone().unwrap_or_default(),
        created: now,
        last_active: now,
    };
    state.save(&features_dir, &feature_name)?;

    // Step 2: Create git worktree (skip branch creation — branch already exists)
    let worktree_path = project_root.join(&feature_name);

    // If the branch already has a registered worktree, handle the conflict.
    // With --from: back up the old worktree and prune so add_worktree can succeed.
    // Without --from: fail with a clear error telling the user to use --from.
    if let Some(existing_wt) = git::find_worktree_for_branch(&main_worktree, branch)? {
        if from.is_some() {
            if existing_wt.exists() {
                let timestamp = Utc::now().format("%Y%m%d%H%M%S");
                let backup = existing_wt.with_extension(format!("bak.{timestamp}"));
                std::fs::rename(&existing_wt, &backup)?;
                eprintln!(
                    "Moved existing worktree {} → {}",
                    existing_wt.display(),
                    backup.display()
                );
            }
            git::prune_worktrees(&main_worktree)?;
        } else {
            return Err(PmError::WorktreeConflict {
                branch: branch.to_string(),
                worktree: existing_wt,
            });
        }
    }

    git::add_worktree(&main_worktree, &worktree_path, branch)?;

    // Step 2.5: Seed Claude Code settings and skills from main worktree
    super::claude_settings::seed_feature_claude(project_root, &worktree_path)?;

    // Step 2.6: Migrate Claude Code sessions from old path if provided.
    // Always use the original --from path for migration since claude sessions
    // are keyed by the original path, not the backup location.
    if let Some(old_path) = from {
        match super::claude_migrate::migrate_sessions(old_path, &worktree_path, claude_base) {
            Ok(msgs) => {
                for msg in msgs {
                    eprintln!("{msg}");
                }
            }
            Err(e) => eprintln!("Warning: Claude session migration failed: {e}"),
        }
    }

    // Step 2.6: Write TASK.md if context provided
    if let Some(ref resolved) = resolved_context {
        std::fs::write(worktree_path.join("TASK.md"), resolved)?;
        git::exclude_pattern(&worktree_path, "TASK.md")?;
    }

    // Step 3: Create tmux session
    let session_name = format!("{project_name}/{feature_name}");
    tmux::create_session(tmux_server, &session_name, &worktree_path)?;

    // Step 3.5: Spawn a claude session to read TASK.md (if context was provided)
    if resolved_context.is_some() {
        let default_agent = &config.agents.default;
        let agent = if default_agent.is_empty() {
            None
        } else {
            Some(default_agent.as_str())
        };
        super::agent_spawn::spawn_claude_session(
            project_root,
            &feature_name,
            agent,
            Some("READ TASK.md"),
            !no_edit,
            None,
            tmux_server,
        )?;
    }

    // Step 4: Update status to wip
    state.status = FeatureStatus::Wip;
    state.last_active = Utc::now();
    state.save(&features_dir, &feature_name)?;

    Ok(feature_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    /// Create a branch on the main worktree so feat_adopt can find it.
    fn create_branch(project_path: &Path, name: &str) {
        let main_worktree = project_path.join("main");
        git::create_branch(&main_worktree, name).unwrap();
    }

    #[test]
    fn feat_adopt_creates_state_file_with_wip_status() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn feat_adopt_creates_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let worktree_path = project_path.join("login");
        assert!(worktree_path.exists());
        assert!(worktree_path.is_dir());
    }

    #[test]
    fn feat_adopt_creates_tmux_session() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        assert!(tmux::has_session(server.name(), &format!("{project_name}/login")).unwrap());
    }

    #[test]
    fn feat_adopt_does_not_create_branch() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        // Branch exists before adopt
        let main_wt = project_path.join("main");
        assert!(git::branch_exists(&main_wt, "login").unwrap());

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        // Branch still exists (not a new one, same one)
        assert!(git::branch_exists(&main_wt, "login").unwrap());
    }

    #[test]
    fn feat_adopt_fails_when_branch_does_not_exist() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());

        let result = feat_adopt(
            &project_path,
            "nonexistent",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        );

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PmError::BranchNotFound(_)));
    }

    #[test]
    fn feat_adopt_fails_when_feature_already_exists() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();
        let result = feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        );

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PmError::FeatureAlreadyExists(_)
        ));
    }

    #[test]
    fn feat_adopt_with_context_writes_task_md() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            Some("Adopt existing login branch"),
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let task_md = project_path.join("login").join("TASK.md");
        assert!(task_md.exists());
        let content = std::fs::read_to_string(&task_md).unwrap();
        assert_eq!(content, "Adopt existing login branch");
    }

    #[test]
    fn feat_adopt_sets_timestamps() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");
        let before = Utc::now();

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert!(state.created >= before);
        assert!(state.last_active >= state.created);
    }

    #[test]
    fn feat_adopt_with_context_creates_claude_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            Some("Adopt existing login branch"),
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        // Session should have 2 windows: the default shell + the claude window
        let output = tmux::list_windows(server.name(), &format!("{project_name}/login")).unwrap();
        assert_eq!(output, 2);
    }

    #[test]
    fn feat_adopt_without_context_has_single_window() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let output = tmux::list_windows(server.name(), &format!("{project_name}/login")).unwrap();
        assert_eq!(output, 1);
    }

    #[test]
    fn feat_adopt_tmux_failure_leaves_initializing_state() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        // Pre-create a tmux session to cause a conflict
        tmux::create_session(server.name(), &format!("{project_name}/login"), dir.path()).unwrap();

        let result = feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        );
        assert!(result.is_err());

        let features_dir = paths::features_dir(&project_path);
        assert!(FeatureState::exists(&features_dir, "login"));
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Initializing);
    }

    #[test]
    fn feat_adopt_with_from_migrates_claude_sessions() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        // Set up fake Claude session data keyed to some old path
        let claude_base = dir.path().join("claude");
        let old_path = std::path::Path::new("/tmp/old-repo");
        let old_key = old_path.to_string_lossy().replace('/', "-");
        let old_session_dir = claude_base.join("projects").join(&old_key);
        std::fs::create_dir_all(&old_session_dir).unwrap();
        std::fs::write(
            old_session_dir.join("session.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", old_path.display()),
        )
        .unwrap();

        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            Some(old_path),
            false,
            server.name(),
            Some(claude_base.as_path()),
        )
        .unwrap();

        // New session dir should exist with updated path
        let worktree_path = project_path.join("login");
        let new_key = worktree_path.to_string_lossy().replace('/', "-");
        let new_session_dir = claude_base.join("projects").join(&new_key);
        assert!(new_session_dir.exists());
        let content = std::fs::read_to_string(new_session_dir.join("session.jsonl")).unwrap();
        assert!(content.contains(&worktree_path.to_string_lossy().to_string()));
        assert!(!content.contains("/tmp/old-repo"));
    }

    #[test]
    fn feat_adopt_slash_branch_sanitizes_feature_name() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        create_branch(&project_path, "ciaran/login");

        feat_adopt(
            &project_path,
            "ciaran/login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        // Feature name should be sanitized
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "ciaran-login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
        assert_eq!(state.branch, "ciaran/login");
        assert_eq!(state.worktree, "ciaran-login");

        // Worktree dir uses sanitized name
        assert!(project_path.join("ciaran-login").exists());

        // Tmux session uses sanitized name
        assert!(tmux::has_session(server.name(), &format!("{project_name}/ciaran-login")).unwrap());
    }

    #[test]
    fn feat_adopt_with_name_override() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, project_name) = server.setup_project(dir.path());
        create_branch(&project_path, "ciaran/eval");

        feat_adopt(
            &project_path,
            "ciaran/eval",
            Some("eval"),
            None,
            None,
            false,
            server.name(),
            None,
        )
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "eval").unwrap();
        assert_eq!(state.branch, "ciaran/eval");
        assert_eq!(state.worktree, "eval");
        assert!(project_path.join("eval").exists());
        assert!(tmux::has_session(server.name(), &format!("{project_name}/eval")).unwrap());
    }

    #[test]
    fn feat_adopt_with_from_handles_existing_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        // Create an existing worktree for the branch (simulating a pre-existing checkout)
        let old_worktree = dir.path().join("old-checkout");
        let main_wt = project_path.join("main");
        git::add_worktree(&main_wt, &old_worktree, "login").unwrap();
        assert!(old_worktree.exists());

        // Set up fake Claude session data keyed to the old worktree path
        let claude_base = dir.path().join("claude");
        let old_key = old_worktree.to_string_lossy().replace('/', "-");
        let old_session_dir = claude_base.join("projects").join(&old_key);
        std::fs::create_dir_all(&old_session_dir).unwrap();
        std::fs::write(
            old_session_dir.join("session.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", old_worktree.display()),
        )
        .unwrap();

        // This should succeed despite the branch already having a worktree
        feat_adopt(
            &project_path,
            "login",
            None,
            None,
            Some(old_worktree.as_path()),
            false,
            server.name(),
            Some(claude_base.as_path()),
        )
        .unwrap();

        // New worktree should exist
        let new_worktree = project_path.join("login");
        assert!(new_worktree.exists());

        // Old worktree should have been moved to a timestamped .bak
        assert!(!old_worktree.exists());
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("old-checkout.bak.")
            })
            .collect();
        assert_eq!(backups.len(), 1, "expected exactly one timestamped backup");

        // Claude sessions should be migrated to the new path
        let new_key = new_worktree.to_string_lossy().replace('/', "-");
        let new_session_dir = claude_base.join("projects").join(&new_key);
        assert!(new_session_dir.exists());
        let content = std::fs::read_to_string(new_session_dir.join("session.jsonl")).unwrap();
        assert!(content.contains(&new_worktree.to_string_lossy().to_string()));

        // Feature state should be Wip
        let features_dir = paths::features_dir(&project_path);
        let state = FeatureState::load(&features_dir, "login").unwrap();
        assert_eq!(state.status, FeatureStatus::Wip);
    }

    #[test]
    fn feat_adopt_fails_with_worktree_conflict_without_from() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project(dir.path());
        create_branch(&project_path, "login");

        // Create an existing worktree for the branch
        let old_worktree = dir.path().join("old-checkout");
        let main_wt = project_path.join("main");
        git::add_worktree(&main_wt, &old_worktree, "login").unwrap();

        // Without --from, should fail with WorktreeConflict
        let result = feat_adopt(
            &project_path,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
            None,
        );

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), PmError::WorktreeConflict { .. }),
            "expected WorktreeConflict error"
        );
    }
}
