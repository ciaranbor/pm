//! Diffing main's settings against a feature's, key by key through nested
//! JSON objects; arrays diff as sets.

use std::path::Path;

use super::{BOLD, DIM, GREEN, RED, RESET, YELLOW, load_file_pairs, settings_files};
use crate::commands::feat_common::require_feature;
use crate::error::Result;
use crate::harness::Harness;

/// Diff main's settings against a feature's settings.
/// Returns a list of human-readable diff lines with ANSI colors. Empty vec means no differences.
pub fn diff(project_root: &Path, feature_name: &str, harness: Harness) -> Result<Vec<String>> {
    let files = settings_files(harness)?;
    require_feature(project_root, feature_name)?;

    let mut lines = Vec::new();
    for pair in load_file_pairs(project_root, feature_name, files, harness)? {
        match (&pair.main, &pair.feature) {
            (None, None) => {}
            (Some(_), None) => {
                lines.push(format!(
                    "{BOLD}{}{RESET}\n  {RED}file only in main{RESET}",
                    pair.filename
                ));
            }
            (None, Some(_)) => {
                lines.push(format!(
                    "{BOLD}{}{RESET}\n  {GREEN}file only in feature{RESET}",
                    pair.filename
                ));
            }
            (Some(m), Some(f)) => {
                if m != f {
                    diff_json(&mut lines, pair.filename, m, f);
                }
            }
        }
    }

    Ok(lines)
}

/// Produce a structured diff of two JSON strings, recursing into nested objects and arrays.
fn diff_json(lines: &mut Vec<String>, filename: &str, main_str: &str, feature: &str) {
    let main_val: std::result::Result<serde_json::Value, _> = serde_json::from_str(main_str);
    let feature_val: std::result::Result<serde_json::Value, _> = serde_json::from_str(feature);

    let (Ok(m), Ok(f)) = (main_val, feature_val) else {
        lines.push(format!(
            "{BOLD}{filename}{RESET}\n  {YELLOW}content differs (not valid JSON objects){RESET}"
        ));
        return;
    };

    let mut file_lines = Vec::new();
    diff_values(&mut file_lines, &m, &f, "");

    if !file_lines.is_empty() {
        lines.push(format!("{BOLD}{filename}{RESET}"));
        lines.extend(file_lines);
    }
}

/// Recursively diff two JSON values, building indented output lines.
fn diff_values(
    lines: &mut Vec<String>,
    main_val: &serde_json::Value,
    feature_val: &serde_json::Value,
    path: &str,
) {
    use serde_json::Value;

    if main_val == feature_val {
        return;
    }

    let indent = if path.is_empty() {
        "  ".to_string()
    } else {
        format!("  {YELLOW}{path}{RESET}\n    ")
    };

    match (main_val, feature_val) {
        (Value::Object(m_map), Value::Object(f_map)) => {
            let mut all_keys: Vec<&String> = m_map.keys().chain(f_map.keys()).collect();
            all_keys.sort();
            all_keys.dedup();

            for key in all_keys {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (m_map.get(key), f_map.get(key)) {
                    (Some(mv), Some(fv)) => {
                        diff_values(lines, mv, fv, &child_path);
                    }
                    (Some(mv), None) => {
                        let val = format_value(mv);
                        lines.push(format!(
                            "  {YELLOW}{child_path}{RESET}\n    {RED}- {val}{RESET}\n    {DIM}(only in main){RESET}"
                        ));
                    }
                    (None, Some(fv)) => {
                        let val = format_value(fv);
                        lines.push(format!(
                            "  {YELLOW}{child_path}{RESET}\n    {GREEN}+ {val}{RESET}\n    {DIM}(only in feature){RESET}"
                        ));
                    }
                    (None, None) => unreachable!(),
                }
            }
        }
        (Value::Array(m_arr), Value::Array(f_arr)) => {
            let only_main: Vec<_> = m_arr.iter().filter(|v| !f_arr.contains(v)).collect();
            let only_feat: Vec<_> = f_arr.iter().filter(|v| !m_arr.contains(v)).collect();

            if !only_main.is_empty() || !only_feat.is_empty() {
                let mut entry = format!("  {YELLOW}{path}{RESET}");
                for v in &only_main {
                    entry.push_str(&format!("\n    {RED}- {}{RESET}", format_value(v)));
                }
                for v in &only_feat {
                    entry.push_str(&format!("\n    {GREEN}+ {}{RESET}", format_value(v)));
                }
                lines.push(entry);
            }
        }
        _ => {
            lines.push(format!(
                "{indent}{RED}- {}{RESET}\n    {GREEN}+ {}{RESET}",
                format_value(main_val),
                format_value(feature_val)
            ));
        }
    }
}

/// Format a JSON value for display — strings without quotes wrapping, others as JSON.
fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
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

    /// Join diff output into a single string for assertions (strips ANSI codes).
    fn diff_output(project: &Path, feature: &str) -> String {
        let lines = diff(project, feature, CC).unwrap();
        let joined = lines.join("\n");
        // Strip ANSI escape sequences for easier assertions
        let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(&joined, "").to_string()
    }

    #[test]
    fn diff_no_differences() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"same":true}"#);
        write_json(&feat_claude, "settings.json", r#"{"same":true}"#);

        let result = diff(&project, "login", CC).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn diff_detects_value_difference() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"key":"a"}"#);
        write_json(&feat_claude, "settings.json", r#"{"key":"b"}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("settings.json"));
        assert!(output.contains("key"));
        assert!(output.contains("- a"));
        assert!(output.contains("+ b"));
    }

    #[test]
    fn diff_detects_key_only_in_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"extra":"val"}"#);
        write_json(&feat_claude, "settings.json", r#"{}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("only in main"));
    }

    #[test]
    fn diff_detects_key_only_in_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{}"#);
        write_json(&feat_claude, "settings.json", r#"{"new_perm":true}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("only in feature"));
    }

    #[test]
    fn diff_file_only_in_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        write_json(&main_claude, "settings.json", r#"{"x":1}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("only in main"));
    }

    #[test]
    fn diff_file_only_in_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"x":1}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("only in feature"));
    }

    #[test]
    fn diff_both_files_missing_no_output() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let result = diff(&project, "login", CC).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn diff_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());

        let result = diff(&project, "nonexistent", CC);
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn diff_ignores_settings_local_json() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");

        let main_claude = paths::main_worktree(&project).join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"same":true}"#);
        write_json(&feat_claude, "settings.json", r#"{"same":true}"#);
        write_json(&main_claude, "settings.local.json", r#"{"env":"prod"}"#);
        write_json(&feat_claude, "settings.local.json", r#"{"env":"dev"}"#);

        assert!(diff(&project, "login", CC).unwrap().is_empty());
    }
}
