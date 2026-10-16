//! Projects and features for a test to act on.

use std::path::{Path, PathBuf};

use super::TestServer;
use crate::commands::init;
use crate::state::project::ProjectEntry;

/// A set-up project: its root, the registry it is registered in, and its
/// name.
type Project = (PathBuf, PathBuf, String);

/// Where `server`'s project in `dir` goes: `dir/<scoped name>`, with a
/// sibling `registry/`.
fn place(server: &TestServer, dir: &Path) -> Project {
    let name = server.scope("myapp");
    (dir.join(&name), dir.join("registry"), name)
}

impl TestServer {
    /// Create a project with init.
    pub fn setup_project(&self, dir: &Path) -> Project {
        let (path, registry, name) = place(self, dir);
        init::init(&path, &registry, None, self.name()).unwrap();
        (path, registry, name)
    }

    /// `setup_project` with the main branch renamed to `master` and recorded
    /// as such, so a test cannot pass by assuming `main`.
    pub fn setup_master_project(&self, dir: &Path) -> Project {
        let (path, registry, name) = self.setup_project(dir);
        let main = crate::state::paths::main_worktree(&path);
        crate::git::run_git(&main, &["branch", "-m", "main", "master"]).unwrap();
        let mut entry = ProjectEntry::load(&registry, &name).unwrap();
        entry.main_branch = "master".to_string();
        entry.save(&registry, &name).unwrap();
        (path, registry, name)
    }

    /// A `master`-default project holding a `child` feature stacked on a
    /// `parent` feature that has since been merged and cleaned up, so
    /// `child`'s base branch no longer exists. Each feature has one commit.
    pub fn setup_orphaned_child(&self, dir: &Path) -> Project {
        use crate::commands::{feat_merge, feat_new};
        let project = self.setup_master_project(dir);
        let (project_path, projects_dir, _) = &project;
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            project_path,
            projects_dir,
            "parent",
            self.name(),
        ))
        .unwrap();
        Self::add_feature_commit(project_path, "parent");
        feat_new::feat_new(&feat_new::FeatNewParams {
            project_root: project_path,
            projects_dir,
            name: "child",
            name_override: None,
            context: None,
            base: Some("parent"),
            workflow: None,
            fresh: None,
            tmux_server: self.name(),
        })
        .unwrap();
        let child = project_path.join("child");
        std::fs::write(child.join("child.txt"), "child work").unwrap();
        crate::git::stage_file(&child, "child.txt").unwrap();
        crate::git::commit(&child, "child work").unwrap();
        feat_merge::feat_merge(project_path, projects_dir, "parent", false, self.name()).unwrap();
        assert!(
            !crate::git::branch_exists(&crate::state::paths::main_worktree(project_path), "parent")
                .unwrap()
        );
        project
    }

    /// Create a project and a feature.
    pub fn setup_project_with_feature(&self, dir: &Path, feature_name: &str) -> Project {
        let (path, registry, name) = self.setup_project(dir);
        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &path,
                &registry,
                feature_name,
                self.name(),
            ),
        )
        .unwrap();
        (path, registry, name)
    }

    /// [`setup_project`](Self::setup_project) without its main tmux
    /// session, for a test that never touches tmux.
    pub fn setup_project_no_tmux(&self, dir: &Path) -> Project {
        let (path, registry, name) = place(self, dir);
        let global = crate::commands::skills::GlobalStore::resolve().unwrap();
        let main_branch = init::populate(&path, &name, &global, None).unwrap();
        init::registry_entry(&path, main_branch, None)
            .save(&registry, &name)
            .unwrap();
        (path, registry, name)
    }

    /// [`setup_project_with_feature`](Self::setup_project_with_feature)
    /// without tmux: the feature's branch, worktree, state and seeded assets
    /// exist, but no session does.
    pub fn setup_project_with_feature_no_tmux(&self, dir: &Path, feature_name: &str) -> Project {
        use crate::state::paths;
        let (path, registry, name) = self.setup_project_no_tmux(dir);
        let main_worktree = paths::main_worktree(&path);
        let worktree_path = path.join(feature_name);

        crate::git::create_branch_from(&main_worktree, feature_name, "main").unwrap();
        crate::git::add_worktree(&main_worktree, &worktree_path, feature_name).unwrap();

        crate::state::feature::FeatureState {
            base: "main".to_string(),
            ..super::feature_state(feature_name)
        }
        .save(&paths::features_dir(&path), feature_name)
        .unwrap();

        crate::commands::seed::seed_feature_assets(&path, &worktree_path).unwrap();

        (path, registry, name)
    }

    /// Add a commit to a feature worktree.
    pub fn add_feature_commit(project_path: &Path, feature_name: &str) {
        let worktree = project_path.join(feature_name);
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        crate::git::stage_file(&worktree, "feature.txt").unwrap();
        crate::git::commit(&worktree, "feature work").unwrap();
    }

    /// Leave a rebase paused in `worktree` with a clean tree: every replayed
    /// commit is followed by a failing `--exec`.
    pub fn pause_rebase(worktree: &Path, onto: &str) {
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

/// A project root at `dir` holding only the directories pm reads, `.pm/features/`
/// and `main/`: no git repo, config or registry entry.
pub fn bare_project(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(crate::state::paths::features_dir(dir)).unwrap();
    std::fs::create_dir_all(crate::state::paths::main_worktree(dir)).unwrap();
    dir.to_path_buf()
}
