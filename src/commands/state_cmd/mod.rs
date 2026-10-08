//! `pm state`: the two git-backed state repos. The project repo is
//! `<project>/.pm/`, where pm controls every file. The global registry repo
//! is the pm config dir: the project registry, global config, and the
//! global workflow tier — minus the bundled workflows, which
//! [`super::state_gitignore`] keeps out of it. Both sync the same way:
//! `push`/`pull` auto-commit whatever `git add -A` finds, so the index is
//! pm's, and a remote is opt-in.

mod backfill;
mod global;
mod init;
mod project;
mod remote;
mod repo;

pub use backfill::{backfill, backfill_with_dir};
pub use global::{
    global_init, global_init_with_remote, global_pull, global_push, global_remote, global_status,
};
pub use project::{init, init_with_remote, pull, push, remote, status, would_init};
pub(crate) use remote::apply_remote_and_pull;

/// Helpers the submodules' tests share.
#[cfg(test)]
mod test_support {
    use crate::git;

    /// Create a bare repo with content pushed to it (simulates a real remote).
    pub(super) fn create_populated_bare(bare_path: &std::path::Path) {
        create_populated_bare_on(bare_path, "main");
    }

    /// Create a bare repo whose only branch is `branch`, with content on it.
    pub(super) fn create_populated_bare_on(bare_path: &std::path::Path, branch: &str) {
        git::init_bare(bare_path).unwrap();

        // Clone, add content, push
        let staging = bare_path.parent().unwrap().join("staging");
        git::clone_repo(&bare_path.to_string_lossy(), &staging).unwrap();
        git::run_git(
            &staging,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        )
        .unwrap();
        std::fs::write(staging.join("remote-file.txt"), "from remote").unwrap();
        git::add_all(&staging).unwrap();
        git::commit_with_message(&staging, "seed remote content").unwrap();
        git::push(&staging, "origin", branch).unwrap();
        // Clean up staging clone
        std::fs::remove_dir_all(&staging).unwrap();
    }
}
