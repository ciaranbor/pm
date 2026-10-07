//! Copying settings files wholesale: `push` overwrites main's with the
//! feature's, `pull` seeds main's into the feature unless its branch tracks
//! them.

use std::path::Path;

use super::{feature_dir, main_dir, settings_files};
use crate::commands::feat_common::require_feature;
use crate::commands::seed::{Seeded, seed_file};
use crate::error::{PmError, Result};
use crate::harness::Harness;
use crate::state::paths;

/// Copy a single settings file from src_dir to dst_dir if it exists in src_dir.
fn copy_settings_file(src_dir: &Path, dst_dir: &Path, filename: &str) -> Result<()> {
    let src = src_dir.join(filename);
    if src.exists() {
        std::fs::create_dir_all(dst_dir)?;
        std::fs::copy(&src, dst_dir.join(filename))?;
    }
    Ok(())
}

/// Push a feature's settings to main.
pub fn push(project_root: &Path, feature_name: &str, harness: Harness) -> Result<()> {
    let files = settings_files(harness)?;
    require_feature(project_root, feature_name)?;

    let src = feature_dir(project_root, feature_name, harness);
    if !src.exists() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no {}/ directory in feature '{feature_name}' at {}",
                harness.config_dir(),
                src.display()
            ),
        )));
    }

    let dst = main_dir(project_root, harness);
    for filename in files {
        copy_settings_file(&src, &dst, filename)?;
    }
    Ok(())
}

/// Pull main's settings into a feature, leaving alone a file the feature's
/// branch tracks (its content reaches or leaves main by merge). Returns one
/// line per file pulled or left alone.
pub fn pull(project_root: &Path, feature_name: &str, harness: Harness) -> Result<Vec<String>> {
    let files = settings_files(harness)?;
    require_feature(project_root, feature_name)?;

    let main = paths::main_worktree(project_root);
    let worktree = project_root.join(feature_name);
    let mut lines = Vec::new();
    for filename in files {
        let rel = Path::new(harness.config_dir()).join(filename);
        let rel_str = rel.display();
        match seed_file(&main, &worktree, &rel, false)? {
            None => {}
            Some(Seeded::Tracked) => lines.push(format!(
                "Left {rel_str} alone: feature '{feature_name}' tracks it in git"
            )),
            Some(Seeded::Deleted) => lines.push(format!(
                "Left {rel_str} absent: feature '{feature_name}' deleted it in git"
            )),
            Some(Seeded::Written) => {
                lines.push(format!("Pulled {rel_str} into feature '{feature_name}'"))
            }
            Some(Seeded::Unchanged) => lines.push(format!("{rel_str} already matches main")),
        }
    }
    if lines.is_empty() {
        lines.push("Main has no settings to pull".to_string());
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::harness_settings::test_support::*;
    use crate::state::paths;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_new_copies_claude_settings_to_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, projects_dir, _) = server.setup_project(dir.path());

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"seeded":true}"#);

        crate::commands::feat_new::feat_new(
            &crate::commands::feat_new::FeatNewParams::with_defaults(
                &project,
                &projects_dir,
                "login",
                server.name(),
            ),
        )
        .unwrap();

        let feat_settings = project.join("login").join(".claude").join("settings.json");
        assert!(feat_settings.exists());
        assert_eq!(
            std::fs::read_to_string(&feat_settings).unwrap(),
            r#"{"seeded":true}"#
        );
    }

    #[test]
    fn pull_does_not_copy_settings_local_json() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"pulled":true}"#);
        write_json(
            &main_claude,
            "settings.local.json",
            r#"{"approvals":"main"}"#,
        );

        pull(&project, "login", CC).unwrap();

        let feat_claude = project.join("login").join(".claude");
        assert_eq!(
            std::fs::read_to_string(feat_claude.join("settings.json")).unwrap(),
            r#"{"pulled":true}"#
        );
        assert!(!feat_claude.join("settings.local.json").exists());
    }

    #[test]
    fn push_copies_settings_json_and_leaves_main_settings_local_json_alone() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(
            &main_claude,
            "settings.local.json",
            r#"{"approvals":"current"}"#,
        );

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"pushed":true}"#);
        write_json(
            &feat_claude,
            "settings.local.json",
            r#"{"approvals":"stale"}"#,
        );

        push(&project, "login", CC).unwrap();

        assert_eq!(
            std::fs::read_to_string(main_claude.join("settings.json")).unwrap(),
            r#"{"pushed":true}"#
        );
        assert_eq!(
            std::fs::read_to_string(main_claude.join("settings.local.json")).unwrap(),
            r#"{"approvals":"current"}"#
        );
    }

    #[test]
    fn push_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let result = push(&project, "nonexistent", CC);
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn push_fails_when_feature_has_no_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        // Ensure no .claude/ dir in feature
        let feat_claude = project.join("login").join(".claude");
        if feat_claude.exists() {
            std::fs::remove_dir_all(&feat_claude).unwrap();
        }

        let result = push(&project, "login", CC);
        assert!(result.is_err());
    }

    #[test]
    fn pull_copies_main_to_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"pulled":true}"#);

        pull(&project, "login", CC).unwrap();

        let feat_claude = project.join("login").join(".claude");
        assert_eq!(
            std::fs::read_to_string(feat_claude.join("settings.json")).unwrap(),
            r#"{"pulled":true}"#
        );
    }

    #[test]
    fn pull_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let result = pull(&project, "nonexistent", CC);
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn pull_leaves_a_settings_file_the_branch_tracks() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let feature = project.join("login");
        write_json(
            &feature.join(".claude"),
            "settings.json",
            r#"{"branch":true}"#,
        );
        crate::git::stage_file(&feature, ".claude/settings.json").unwrap();
        crate::git::commit(&feature, "track settings").unwrap();
        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"main":true}"#);

        let lines = pull(&project, "login", CC).unwrap();

        assert_eq!(
            std::fs::read_to_string(feature.join(".claude/settings.json")).unwrap(),
            r#"{"branch":true}"#
        );
        assert_eq!(
            lines,
            ["Left .claude/settings.json alone: feature 'login' tracks it in git"]
        );
    }

    #[test]
    fn pull_without_main_settings_is_a_no_op() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        // Strip the Stop-hook settings.json seeded by `pm init`.
        let _ = std::fs::remove_dir_all(paths::main_worktree(&project).join(".claude"));

        assert_eq!(
            pull(&project, "login", CC).unwrap(),
            ["Main has no settings to pull"]
        );
    }

    #[test]
    fn push_overwrites_main_with_feature_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        // Main has old settings
        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"old":true}"#);

        // Feature has new settings
        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"new":true}"#);

        push(&project, "login", CC).unwrap();

        let content = std::fs::read_to_string(main_claude.join("settings.json")).unwrap();
        assert_eq!(content, r#"{"new":true}"#);
    }

    #[test]
    fn pull_overwrites_feature_with_main_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        // Feature has diverged settings
        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"diverged":true}"#);

        // Main has canonical settings
        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"canonical":true}"#);

        pull(&project, "login", CC).unwrap();

        let content = std::fs::read_to_string(feat_claude.join("settings.json")).unwrap();
        assert_eq!(content, r#"{"canonical":true}"#);
    }
}
