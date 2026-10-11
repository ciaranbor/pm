//! Printing a scope's settings files.

use std::path::Path;

use super::{BOLD, RESET, settings_dir, settings_files};
use crate::error::Result;
use crate::harness::Harness;

/// The contents of the settings files in `scope`'s worktree.
pub fn list(project_root: &Path, scope: &str, harness: Harness) -> Result<Vec<String>> {
    list_settings_dir(
        &settings_dir(project_root, scope, harness),
        settings_files(harness)?,
    )
}

fn list_settings_dir(dir: &Path, files: &[&str]) -> Result<Vec<String>> {
    let mut lines = Vec::new();

    for &filename in files {
        let path = dir.join(filename);
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("{BOLD}{filename}{RESET}"));
            for line in content.lines() {
                lines.push(line.to_string());
            }
        }
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
    fn list_shows_main_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(
            &main_claude,
            "settings.json",
            "{\n  \"permissions\": true\n}",
        );

        let lines = list(&project, "main", CC).unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(output.contains("settings.json"));
        assert!(output.contains("\"permissions\": true"));
    }

    #[test]
    fn list_skips_main_settings_local_json() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"a":1}"#);
        write_json(&main_claude, "settings.local.json", r#"{"b":2}"#);

        let lines = list(&project, "main", CC).unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(output.contains("settings.json"));
        assert!(!output.contains("settings.local.json"));
        assert!(!output.contains(r#""b":2"#));
    }

    #[test]
    fn list_returns_empty_for_main_when_no_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        // `pm init` now installs the Stop hook into main/.claude/settings.json;
        // strip it to exercise the "no .claude/ dir" branch.
        let _ = std::fs::remove_dir_all(paths::main_worktree(&project).join(".claude"));

        let lines = list(&project, "main", CC).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn list_shows_feature_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(
            &feat_claude,
            "settings.json",
            "{\n  \"permissions\": true\n}",
        );

        let lines = list(&project, "login", CC).unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(output.contains("settings.json"));
        assert!(output.contains("\"permissions\": true"));
    }

    #[test]
    fn list_skips_settings_local_json() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"a":1}"#);
        write_json(&feat_claude, "settings.local.json", r#"{"b":2}"#);

        let lines = list(&project, "login", CC).unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(output.contains("settings.json"));
        assert!(!output.contains("settings.local.json"));
        assert!(!output.contains(r#""b":2"#));
    }

    #[test]
    fn list_returns_empty_when_no_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        // Remove .claude/ if seeded
        let feat_claude = project.join("login").join(".claude");
        if feat_claude.exists() {
            std::fs::remove_dir_all(&feat_claude).unwrap();
        }

        let lines = list(&project, "login", CC).unwrap();
        assert!(lines.is_empty());
    }
}
