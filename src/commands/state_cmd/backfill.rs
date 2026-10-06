//! Filling registry entries' remote URLs from the repos on disk.

use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::state::paths;

/// Backfill `repo_url` and `state_remote` for all registry entries by reading
/// the actual git remotes from each project's main worktree and .pm/ directory.
pub fn backfill() -> Result<Vec<String>> {
    let projects_dir = paths::global_projects_dir()?;
    backfill_with_dir(&projects_dir)
}

/// Testable inner function that takes an explicit projects directory.
pub fn backfill_with_dir(projects_dir: &Path) -> Result<Vec<String>> {
    let projects = crate::state::project::ProjectEntry::list(projects_dir)?;
    let mut messages = Vec::new();

    for (name, mut entry) in projects {
        // Flag relative roots before trying to use them. A bare path like
        // `exo-bench` would be resolved against the *caller's* CWD on every
        // load, silently corrupting cross-project messaging. We can't fix it
        // automatically (we don't know the original CWD), so we report and
        // skip — the user must `pm delete` + re-register from the right dir.
        if !crate::path_utils::is_portable(&entry.root) {
            messages.push(format!(
                "{name}: WARNING relative root \"{}\" — delete entry and re-register from the correct directory",
                entry.root
            ));
            continue;
        }

        let root = entry.root_path();
        if !root.exists() {
            messages.push(format!("{name}: skipped (root does not exist)"));
            continue;
        }

        let mut changed = false;

        // Backfill repo_url from main worktree's origin
        if entry.repo_url.is_none() {
            let main_path = paths::main_worktree(&root);
            if git::is_git_repo(&main_path)
                && let Ok(Some(url)) = git::remote_url(&main_path, "origin")
            {
                entry.repo_url = Some(url.clone());
                changed = true;
                messages.push(format!("{name}: set repo_url = {url}"));
            }
        }

        // Backfill state_remote from .pm/'s origin
        if entry.state_remote.is_none() {
            let pm_dir = paths::pm_dir(&root);
            if git::is_git_repo(&pm_dir)
                && let Ok(Some(url)) = git::remote_url(&pm_dir, "origin")
            {
                entry.state_remote = Some(url.clone());
                changed = true;
                messages.push(format!("{name}: set state_remote = {url}"));
            }
        }

        if changed {
            entry.save(projects_dir, &name)?;
        } else {
            messages.push(format!("{name}: nothing to backfill"));
        }
    }

    if messages.is_empty() {
        messages.push("No projects in registry".to_string());
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::state::project::ProjectEntry;
    use tempfile::tempdir;

    /// Create a project dir with a main worktree that has a git origin remote.
    fn setup_project_with_origin(root: &std::path::Path, origin_url: &str) {
        let main_path = paths::main_worktree(root);
        std::fs::create_dir_all(&main_path).unwrap();
        git::init_repo(&main_path).unwrap();
        git::add_remote(&main_path, "origin", origin_url).unwrap();
    }

    /// Create a .pm/ dir with a git repo and origin remote.
    fn setup_pm_with_remote(root: &std::path::Path, remote_url: &str) {
        let pm_dir = root.join(".pm");
        std::fs::create_dir_all(pm_dir.join("features")).unwrap();
        git::init_repo(&pm_dir).unwrap();
        git::add_remote(&pm_dir, "origin", remote_url).unwrap();
    }

    #[test]
    fn backfill_fills_repo_url_from_origin() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let project_root = dir.path().join("myapp");

        setup_project_with_origin(&project_root, "https://github.com/user/myapp.git");

        let entry = ProjectEntry {
            root: project_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(msgs.iter().any(|m| m.contains("set repo_url")), "{msgs:?}");

        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(
            loaded.repo_url.as_deref(),
            Some("https://github.com/user/myapp.git")
        );
    }

    #[test]
    fn backfill_fills_state_remote_from_pm_origin() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let project_root = dir.path().join("myapp");

        std::fs::create_dir_all(&project_root).unwrap();
        setup_pm_with_remote(&project_root, "https://github.com/user/myapp-pm-state.git");

        let entry = ProjectEntry {
            root: project_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("set state_remote")),
            "{msgs:?}"
        );

        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(
            loaded.state_remote.as_deref(),
            Some("https://github.com/user/myapp-pm-state.git")
        );
    }

    #[test]
    fn backfill_skips_entries_already_with_urls() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let project_root = dir.path().join("myapp");

        setup_project_with_origin(&project_root, "https://github.com/user/myapp.git");

        let entry = ProjectEntry {
            root: project_root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: Some("https://existing.com/repo.git".to_string()),
            state_remote: Some("https://existing.com/state.git".to_string()),
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("nothing to backfill")),
            "{msgs:?}"
        );

        // URLs should be unchanged
        let loaded = ProjectEntry::load(&projects_dir, "myapp").unwrap();
        assert_eq!(
            loaded.repo_url.as_deref(),
            Some("https://existing.com/repo.git")
        );
    }

    #[test]
    fn backfill_skips_missing_root() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        let entry = ProjectEntry {
            root: "/nonexistent/path/myapp".to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(&projects_dir, "myapp").unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.contains("skipped (root does not exist)")),
            "{msgs:?}"
        );
    }

    #[test]
    fn backfill_warns_on_relative_root() {
        // The `pm init exo-bench` (relative path) bug saved entries with
        // non-portable roots. Backfill cannot auto-fix these (we don't know
        // the original CWD), but it must surface them so the user can
        // delete + re-register.
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        // Bypass ProjectEntry::save validation by writing the toml directly.
        // This simulates an entry created by the buggy code path.
        std::fs::write(
            projects_dir.join("exo-bench.toml"),
            "root = \"exo-bench\"\nmain_branch = \"main\"\n",
        )
        .unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(
            msgs.iter().any(|m| m.contains("exo-bench")
                && m.contains("WARNING")
                && m.contains("relative")),
            "expected relative-root warning, got: {msgs:?}"
        );
    }

    #[test]
    fn backfill_empty_registry() {
        let dir = tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let msgs = backfill_with_dir(&projects_dir).unwrap();
        assert!(msgs.iter().any(|m| m.contains("No projects")), "{msgs:?}");
    }
}
