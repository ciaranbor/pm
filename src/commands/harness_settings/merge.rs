//! Merging a feature's settings into main's: a union of object keys and
//! array items, with one side winning scalar conflicts.

use std::path::Path;

use super::{load_file_pairs, main_dir, settings_files};
use crate::commands::feat_common::require_feature;
use crate::error::Result;
use crate::harness::Harness;

/// Merge main and feature settings with union semantics, writing the result to main.
/// When `ours` is true, the feature (ours) wins on scalar conflicts; otherwise main (theirs)
/// wins. Default should be theirs (main wins).
pub fn merge(project_root: &Path, feature_name: &str, ours: bool, harness: Harness) -> Result<()> {
    let files = settings_files(harness)?;
    require_feature(project_root, feature_name)?;

    let dst = main_dir(project_root, harness);

    for pair in load_file_pairs(project_root, feature_name, files, harness)? {
        let merged = match (pair.main, pair.feature) {
            (None, None) => continue,
            (Some(m), None) => m,
            (None, Some(f)) => f,
            (Some(m), Some(f)) => {
                if m == f {
                    continue;
                }
                merge_json(&m, &f, ours)
            }
        };
        std::fs::create_dir_all(&dst)?;
        std::fs::write(dst.join(pair.filename), merged)?;
    }

    Ok(())
}

/// Merge two JSON strings with union semantics.
/// `ours` controls which side wins on scalar conflicts (true = feature wins).
fn merge_json(main_str: &str, feature: &str, ours: bool) -> String {
    let m_val: std::result::Result<serde_json::Value, _> = serde_json::from_str(main_str);
    let f_val: std::result::Result<serde_json::Value, _> = serde_json::from_str(feature);

    match (m_val, f_val) {
        (Ok(m), Ok(f)) => {
            let merged = merge_values(m, f, ours);
            serde_json::to_string_pretty(&merged).unwrap_or_else(|_| feature.to_string())
        }
        // If either side isn't valid JSON, the winning side takes all
        _ => {
            if ours {
                feature.to_string()
            } else {
                main_str.to_string()
            }
        }
    }
}

/// Recursively merge two JSON values with union semantics.
fn merge_values(
    main_val: serde_json::Value,
    feature: serde_json::Value,
    ours: bool,
) -> serde_json::Value {
    use serde_json::Value;

    match (main_val, feature) {
        // Objects: union of keys, recurse on shared keys
        (Value::Object(mut m_map), Value::Object(f_map)) => {
            for (key, f_val) in f_map {
                if let Some(m_val) = m_map.remove(&key) {
                    m_map.insert(key, merge_values(m_val, f_val, ours));
                } else {
                    m_map.insert(key, f_val);
                }
            }
            Value::Object(m_map)
        }
        // Arrays: union (deduplicated, preserving order)
        (Value::Array(m_arr), Value::Array(f_arr)) => {
            let mut merged = m_arr;
            for item in f_arr {
                if !merged.contains(&item) {
                    merged.push(item);
                }
            }
            Value::Array(merged)
        }
        // Scalar conflict: ours (feature) or theirs (main) wins
        (m, f) => {
            if ours {
                f
            } else {
                m
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::harness_settings::test_support::*;
    use crate::error::PmError;
    use crate::state::paths;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn read_merged(project: &Path, filename: &str) -> serde_json::Value {
        let main_claude = paths::main_worktree(project).join(".claude");
        let content = std::fs::read_to_string(main_claude.join(filename)).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn merge_unions_object_keys() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"a":1}"#);
        write_json(&feat_claude, "settings.json", r#"{"b":2}"#);

        merge(&project, "login", true, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["a"], 1);
        assert_eq!(result["b"], 2);
    }

    #[test]
    fn merge_unions_arrays() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(
            &main_claude,
            "settings.json",
            r#"{"perms":["read","write"]}"#,
        );
        write_json(
            &feat_claude,
            "settings.json",
            r#"{"perms":["write","exec"]}"#,
        );

        merge(&project, "login", true, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        let perms: Vec<&str> = result["perms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(perms, vec!["read", "write", "exec"]);
    }

    #[test]
    fn merge_default_theirs_main_wins_on_scalar_conflict() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"mode":"strict"}"#);
        write_json(&feat_claude, "settings.json", r#"{"mode":"relaxed"}"#);

        // ours=false is the default (theirs/main wins)
        merge(&project, "login", false, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["mode"], "strict");
    }

    #[test]
    fn merge_ours_feature_wins_on_scalar_conflict() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"mode":"strict"}"#);
        write_json(&feat_claude, "settings.json", r#"{"mode":"relaxed"}"#);

        merge(&project, "login", true, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["mode"], "relaxed");
    }

    #[test]
    fn merge_only_feature_exists_copies_to_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"new":true}"#);

        merge(&project, "login", false, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["new"], true);
    }

    #[test]
    fn merge_only_main_exists_keeps_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"existing":true}"#);

        merge(&project, "login", false, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["existing"], true);
    }

    #[test]
    fn merge_neither_exists_is_noop() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        // Strip the Stop-hook settings.json seeded by `pm init` (and the
        // feature copy seeded by seed::seed_feature_assets during feat_new).
        let _ = std::fs::remove_dir_all(paths::main_worktree(&project).join(".claude"));
        let _ = std::fs::remove_dir_all(project.join("login").join(".claude"));

        merge(&project, "login", false, CC).unwrap();

        let main_claude = paths::main_worktree(&project).join(".claude");
        assert!(!main_claude.join("settings.json").exists());
    }

    #[test]
    fn merge_identical_files_is_noop() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"same":true}"#);
        write_json(&feat_claude, "settings.json", r#"{"same":true}"#);

        let before = std::fs::metadata(main_claude.join("settings.json"))
            .unwrap()
            .modified()
            .unwrap();

        merge(&project, "login", false, CC).unwrap();

        let after = std::fs::metadata(main_claude.join("settings.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn merge_recurses_into_nested_objects() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(
            &main_claude,
            "settings.json",
            r#"{"outer":{"m_key":"m_val"}}"#,
        );
        write_json(
            &feat_claude,
            "settings.json",
            r#"{"outer":{"f_key":"f_val"}}"#,
        );

        merge(&project, "login", false, CC).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["outer"]["m_key"], "m_val");
        assert_eq!(result["outer"]["f_key"], "f_val");
    }

    #[test]
    fn merge_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let result = merge(&project, "nonexistent", false, CC);
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn merge_malformed_json_winner_takes_all() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", "not json");
        write_json(&feat_claude, "settings.json", r#"{"valid":true}"#);

        // Default (ours=false) → main wins
        merge(&project, "login", false, CC).unwrap();
        let content = std::fs::read_to_string(main_claude.join("settings.json")).unwrap();
        assert_eq!(content, "not json");

        // ours=true → feature wins
        merge(&project, "login", true, CC).unwrap();
        let content = std::fs::read_to_string(main_claude.join("settings.json")).unwrap();
        assert_eq!(content, r#"{"valid":true}"#);
    }

    #[test]
    fn merge_leaves_settings_local_json_untouched() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.local.json", r#"{"a":1}"#);
        write_json(&feat_claude, "settings.local.json", r#"{"b":2}"#);

        merge(&project, "login", false, CC).unwrap();

        assert_eq!(
            std::fs::read_to_string(main_claude.join("settings.local.json")).unwrap(),
            r#"{"a":1}"#
        );
    }
}
