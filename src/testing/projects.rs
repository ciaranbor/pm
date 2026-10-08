//! Projects and features for a test to act on.

use super::TestServer;

impl TestServer {
    /// Create a project with init, returning `(project_path, projects_dir, project_name)`.
    pub fn setup_project(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let name = self.scope("myapp");
        let project_path = dir.join(&name);
        let projects_dir = dir.join("registry");
        crate::commands::init::init(&project_path, &projects_dir, None, self.name()).unwrap();
        (project_path, projects_dir, name)
    }

    /// `setup_project` with the main branch renamed to `master` and recorded
    /// as such, so a test cannot pass by assuming `main`.
    pub fn setup_master_project(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let (project_path, projects_dir, project_name) = self.setup_project(dir);
        let main = crate::state::paths::main_worktree(&project_path);
        crate::git::rename_branch(&main, "main", "master").unwrap();
        let mut entry =
            crate::state::project::ProjectEntry::load(&projects_dir, &project_name).unwrap();
        entry.main_branch = "master".to_string();
        entry.save(&projects_dir, &project_name).unwrap();
        (project_path, projects_dir, project_name)
    }

    /// A `master`-default project holding a `child` feature stacked on a
    /// `parent` feature that has since been merged and cleaned up, so
    /// `child`'s base branch no longer exists. Each feature has one commit.
    pub fn setup_orphaned_child(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        use crate::commands::{feat_merge, feat_new};
        let (project_path, projects_dir, _) = self.setup_master_project(dir);
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "parent",
            self.name(),
        ))
        .unwrap();
        Self::add_feature_commit(&project_path, "parent");
        feat_new::feat_new(&feat_new::FeatNewParams {
            project_root: &project_path,
            projects_dir: &projects_dir,
            name: "child",
            name_override: None,
            context: None,
            base: Some("parent"),
            workflow: None,
            tmux_server: self.name(),
        })
        .unwrap();
        let child = project_path.join("child");
        std::fs::write(child.join("child.txt"), "child work").unwrap();
        crate::git::stage_file(&child, "child.txt").unwrap();
        crate::git::commit(&child, "child work").unwrap();
        feat_merge::feat_merge(&project_path, &projects_dir, "parent", false, self.name()).unwrap();
        assert!(
            !crate::git::branch_exists(
                &crate::state::paths::main_worktree(&project_path),
                "parent"
            )
            .unwrap()
        );
        (project_path, projects_dir)
    }

    /// Create a project and a feature, returning `(project_path, project_name)`.
    pub fn setup_project_with_feature(
        &self,
        dir: &std::path::Path,
        feature_name: &str,
    ) -> (std::path::PathBuf, String) {
        let (project_path, projects_dir, project_name) = self.setup_project(dir);
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project_path,
                &projects_dir,
                feature_name,
                self.name(),
            ),
        )
        .unwrap();
        (project_path, project_name)
    }

    /// Create a project without tmux sessions.
    ///
    /// Replicates the filesystem structure of `pm init` (git repo, `.pm/`
    /// directory, config, hooks, global asset tier, registry entry) but skips
    /// creating the tmux session. Use this for tests that only need the project
    /// directory layout and never interact with tmux.
    ///
    /// Keep in sync with `commands::init::init()` — if init gains new
    /// setup steps, they should be mirrored here.
    pub fn setup_project_no_tmux(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        use crate::state::paths;
        let name = self.scope("myapp");
        let project_path = dir.join(&name);
        let projects_dir = dir.join("registry");

        // Replicate init's filesystem work without tmux
        std::fs::create_dir_all(&project_path).unwrap();
        let main_path = paths::main_worktree(&project_path);
        crate::git::init_repo(&main_path).unwrap();

        let pm_dir = paths::pm_dir(&project_path);
        let features_dir = paths::features_dir(&project_path);
        std::fs::create_dir_all(&features_dir).unwrap();

        // Write project config
        use crate::state::project::{AgentsConfig, ProjectConfig, ProjectInfo};
        let config = ProjectConfig {
            project: ProjectInfo {
                name: name.clone(),
                max_features: None,
            },
            agents: AgentsConfig::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();

        crate::hooks::bootstrap(&project_path).unwrap();
        crate::commands::docs::bootstrap(&project_path).unwrap();
        crate::commands::state_cmd::init(&project_path).unwrap();
        crate::commands::hooks_install::install(Some(&project_path)).unwrap();
        crate::commands::skills::install_global().unwrap();
        crate::commands::skills::write_migration_marker(&project_path).unwrap();
        crate::commands::vanilla_rename::write_marker(&project_path).unwrap();

        // Register in global registry
        use crate::state::project::ProjectEntry;
        let entry = ProjectEntry {
            root: crate::path_utils::to_portable(&project_path),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, &name).unwrap();

        (project_path, projects_dir, name)
    }

    /// Create a project and a feature without tmux sessions.
    ///
    /// Same as `setup_project_with_feature` but skips all tmux interaction.
    /// The git branch and worktree are created, and feature state is written,
    /// but no tmux sessions exist for the project or feature.
    pub fn setup_project_with_feature_no_tmux(
        &self,
        dir: &std::path::Path,
        feature_name: &str,
    ) -> (std::path::PathBuf, String) {
        use crate::state::paths;
        let (project_path, _, project_name) = self.setup_project_no_tmux(dir);
        let main_worktree = paths::main_worktree(&project_path);
        let worktree_path = project_path.join(feature_name);

        // Create git branch + worktree
        crate::git::create_branch_from(&main_worktree, feature_name, "main").unwrap();
        crate::git::add_worktree(&main_worktree, &worktree_path, feature_name).unwrap();

        // Write feature state
        let features_dir = paths::features_dir(&project_path);
        use crate::state::feature::{FeatureState, FeatureStatus};
        let now = chrono::Utc::now();
        let state = FeatureState {
            branch: feature_name.to_string(),
            worktree: feature_name.to_string(),
            status: FeatureStatus::Wip,
            pr: String::new(),
            base: "main".to_string(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
            team: Default::default(),
        };
        state.save(&features_dir, feature_name).unwrap();

        crate::commands::seed::seed_feature_assets(&project_path, &worktree_path).unwrap();

        (project_path, project_name)
    }

    /// Add a commit to a feature worktree.
    pub fn add_feature_commit(project_path: &std::path::Path, feature_name: &str) {
        let worktree = project_path.join(feature_name);
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        crate::git::stage_file(&worktree, "feature.txt").unwrap();
        crate::git::commit(&worktree, "feature work").unwrap();
    }

    /// Leave a rebase paused in `worktree` with a clean tree: every replayed
    /// commit is followed by a failing `--exec`.
    pub fn pause_rebase(worktree: &std::path::Path, onto: &str) {
        let status = std::process::Command::new("git")
            .current_dir(worktree)
            .args(["rebase", "--exec", "false", onto])
            .output()
            .unwrap()
            .status;
        assert!(!status.success());
        assert!(crate::git::rebase_in_progress(worktree).unwrap());
    }
}
