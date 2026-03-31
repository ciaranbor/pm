use std::sync::OnceLock;
/// Test utilities shared across modules.
use std::sync::atomic::{AtomicU32, Ordering};

static TMUX_SERVER_COUNTER: AtomicU32 = AtomicU32::new(0);
static SHARED_SERVER_NAME: OnceLock<String> = OnceLock::new();

fn shared_server_name() -> &'static str {
    SHARED_SERVER_NAME.get_or_init(|| {
        let name = format!("pm-test-{}", std::process::id());
        // Kill any stale server from a previous process with the same PID.
        let _ = crate::tmux::kill_server(Some(&name));
        name
    })
}

/// RAII guard for a shared tmux test server. All tests share a single tmux
/// server process (one per cargo-test binary) to minimise pty usage. Each
/// `TestServer` instance gets a unique prefix so session names don't collide
/// across parallel tests. On drop, only sessions belonging to this instance
/// are killed — the shared server stays alive for other tests.
pub struct TestServer {
    prefix: String,
}

impl Default for TestServer {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServer {
    pub fn new() -> Self {
        let id = TMUX_SERVER_COUNTER.fetch_add(1, Ordering::SeqCst);
        Self {
            prefix: format!("t{id}"),
        }
    }

    /// Get the shared server name to pass to tmux functions as `Some(&str)`.
    pub fn name(&self) -> Option<&str> {
        Some(shared_server_name())
    }

    /// Return a name scoped to this test instance. Use this for project names
    /// and direct session names to avoid collisions with other parallel tests.
    pub fn scope(&self, name: &str) -> String {
        format!("{}-{}", self.prefix, name)
    }

    /// Create a project with init, returning `(project_path, projects_dir, project_name)`.
    pub fn setup_project(
        &self,
        dir: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let name = self.scope("myapp");
        let project_path = dir.join(&name);
        let projects_dir = dir.join("registry");
        crate::commands::init::init(&project_path, &projects_dir, self.name()).unwrap();
        (project_path, projects_dir, name)
    }

    /// Create a project and a feature, returning `(project_path, project_name)`.
    pub fn setup_project_with_feature(
        &self,
        dir: &std::path::Path,
        feature_name: &str,
    ) -> (std::path::PathBuf, String) {
        let (project_path, _, project_name) = self.setup_project(dir);
        crate::commands::feat_new::feat_new(
            &project_path,
            feature_name,
            None,
            None,
            None,
            false,
            self.name(),
        )
        .unwrap();
        (project_path, project_name)
    }

    /// Add a commit to a feature worktree.
    pub fn add_feature_commit(project_path: &std::path::Path, feature_name: &str) {
        let worktree = project_path.join(feature_name);
        std::fs::write(worktree.join("feature.txt"), "feature work").unwrap();
        crate::git::stage_file(&worktree, "feature.txt").unwrap();
        crate::git::commit(&worktree, "feature work").unwrap();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Kill only sessions whose name starts with our prefix
        let prefix = format!("{}-", self.prefix);
        if let Ok(sessions) = crate::tmux::list_sessions(self.name()) {
            for s in sessions {
                if s.starts_with(&prefix) {
                    let _ = crate::tmux::kill_session(self.name(), &s);
                }
            }
        }
    }
}
