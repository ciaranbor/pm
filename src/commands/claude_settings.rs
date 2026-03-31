use std::path::Path;

use crate::error::{PmError, Result};
use crate::state::feature::FeatureState;
use crate::state::paths;

const SETTINGS_FILES: &[&str] = &["settings.json", "settings.local.json"];

/// Copy a single settings file from src_dir to dst_dir if it exists in src_dir.
fn copy_settings_file(src_dir: &Path, dst_dir: &Path, filename: &str) -> Result<()> {
    let src = src_dir.join(filename);
    if src.exists() {
        std::fs::create_dir_all(dst_dir)?;
        std::fs::copy(&src, dst_dir.join(filename))?;
    }
    Ok(())
}

fn main_claude_dir(project_root: &Path) -> std::path::PathBuf {
    project_root.join("main").join(".claude")
}

/// A pair of optional file contents (main, feature) for a single settings file.
struct FilePair {
    filename: &'static str,
    main: Option<String>,
    feature: Option<String>,
}

/// Load both sides (main + feature) for each settings file.
fn load_file_pairs(project_root: &Path, feature_name: &str) -> Result<Vec<FilePair>> {
    let main_dir = main_claude_dir(project_root);
    let feature_dir = project_root.join(feature_name).join(".claude");

    let mut pairs = Vec::new();
    for &filename in SETTINGS_FILES {
        let main_path = main_dir.join(filename);
        let feature_path = feature_dir.join(filename);

        let main_content = if main_path.exists() {
            Some(std::fs::read_to_string(&main_path)?)
        } else {
            None
        };
        let feature_content = if feature_path.exists() {
            Some(std::fs::read_to_string(&feature_path)?)
        } else {
            None
        };

        pairs.push(FilePair {
            filename,
            main: main_content,
            feature: feature_content,
        });
    }
    Ok(pairs)
}

pub fn require_feature(project_root: &Path, feature_name: &str) -> Result<()> {
    let features_dir = paths::features_dir(project_root);
    if !FeatureState::exists(&features_dir, feature_name) {
        return Err(PmError::FeatureNotFound(feature_name.to_string()));
    }
    Ok(())
}

use crate::fs_utils::copy_dir_recursive;

/// Called during `feat new` to seed the new feature with the project's settings and skills.
pub fn seed_feature_claude(project_root: &Path, feature_worktree: &Path) -> Result<()> {
    let src = main_claude_dir(project_root);
    if !src.exists() {
        return Ok(());
    }
    let dst = feature_worktree.join(".claude");
    for filename in SETTINGS_FILES {
        copy_settings_file(&src, &dst, filename)?;
    }
    // Copy skills directory if it exists
    let skills_src = src.join("skills");
    if skills_src.is_dir() {
        copy_dir_recursive(&skills_src, &dst.join("skills"))?;
    }
    Ok(())
}

/// List a feature's Claude Code settings by displaying the contents of its `.claude/` settings files.
pub fn list(project_root: &Path, feature_name: &str) -> Result<Vec<String>> {
    require_feature(project_root, feature_name)?;

    let feature_claude_dir = project_root.join(feature_name).join(".claude");
    let mut lines = Vec::new();

    for &filename in SETTINGS_FILES {
        let path = feature_claude_dir.join(filename);
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

/// Push a feature's `.claude/` settings to main's `.claude/`.
pub fn push(project_root: &Path, feature_name: &str) -> Result<()> {
    require_feature(project_root, feature_name)?;

    let feature_claude_dir = project_root.join(feature_name).join(".claude");
    if !feature_claude_dir.exists() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no .claude/ directory in feature '{feature_name}' at {}",
                feature_claude_dir.display()
            ),
        )));
    }

    let dst = main_claude_dir(project_root);
    for filename in SETTINGS_FILES {
        copy_settings_file(&feature_claude_dir, &dst, filename)?;
    }
    Ok(())
}

/// Pull main's `.claude/` settings into a feature's `.claude/` directory.
pub fn pull(project_root: &Path, feature_name: &str) -> Result<()> {
    require_feature(project_root, feature_name)?;

    let src = main_claude_dir(project_root);
    if !src.exists() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no .claude/ directory in main at {}", src.display()),
        )));
    }

    let feature_claude_dir = project_root.join(feature_name).join(".claude");
    for filename in SETTINGS_FILES {
        copy_settings_file(&src, &feature_claude_dir, filename)?;
    }
    Ok(())
}

// ANSI color helpers
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Diff main's settings against a feature's settings.
/// Returns a list of human-readable diff lines with ANSI colors. Empty vec means no differences.
pub fn diff(project_root: &Path, feature_name: &str) -> Result<Vec<String>> {
    require_feature(project_root, feature_name)?;

    let mut lines = Vec::new();
    for pair in load_file_pairs(project_root, feature_name)? {
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

/// Merge main and feature settings with union semantics, writing result to main's `.claude/`.
/// When `ours` is true, the feature (ours) wins on scalar conflicts; otherwise main (theirs)
/// wins. Default should be theirs (main wins).
pub fn merge(project_root: &Path, feature_name: &str, ours: bool) -> Result<()> {
    require_feature(project_root, feature_name)?;

    let dst = main_claude_dir(project_root);

    for pair in load_file_pairs(project_root, feature_name)? {
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
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn write_json(dir: &Path, filename: &str, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(filename), content).unwrap();
    }

    /// Strip ANSI escape sequences for easier assertions.
    fn strip_ansi(s: &str) -> String {
        let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(s, "").to_string()
    }

    // --- list ---

    #[test]
    fn list_shows_feature_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(
            &feat_claude,
            "settings.json",
            "{\n  \"permissions\": true\n}",
        );

        let lines = list(&project, "login").unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(output.contains("settings.json"));
        assert!(output.contains("\"permissions\": true"));
    }

    #[test]
    fn list_shows_both_files() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"a":1}"#);
        write_json(&feat_claude, "settings.local.json", r#"{"b":2}"#);

        let lines = list(&project, "login").unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(output.contains("settings.json"));
        assert!(output.contains("settings.local.json"));
    }

    #[test]
    fn list_returns_empty_when_no_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        // Remove .claude/ if seeded
        let feat_claude = project.join("login").join(".claude");
        if feat_claude.exists() {
            std::fs::remove_dir_all(&feat_claude).unwrap();
        }

        let lines = list(&project, "login").unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn list_only_local_settings_no_separator() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        // Remove settings.json if seeded, keep only settings.local.json
        let _ = std::fs::remove_file(feat_claude.join("settings.json"));
        write_json(&feat_claude, "settings.local.json", r#"{"local":true}"#);

        let lines = list(&project, "login").unwrap();
        let output = strip_ansi(&lines.join("\n"));
        assert!(!output.contains("settings.json\n\n")); // no blank separator before first file
        assert!(output.contains("settings.local.json"));
        assert!(output.contains("\"local\":true"));
    }

    #[test]
    fn list_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let result = list(&project, "nonexistent");
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    // --- seed_feature_claude ---

    #[test]
    fn seed_copies_settings_from_main_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"permissions":true}"#);
        write_json(&main_claude, "settings.local.json", r#"{"local":true}"#);

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_claude(&project, &feature_wt).unwrap();

        let dst = feature_wt.join(".claude");
        assert_eq!(
            std::fs::read_to_string(dst.join("settings.json")).unwrap(),
            r#"{"permissions":true}"#
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("settings.local.json")).unwrap(),
            r#"{"local":true}"#
        );
    }

    #[test]
    fn seed_noop_when_no_main_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_claude(&project, &feature_wt).unwrap();

        assert!(!feature_wt.join(".claude").exists());
    }

    #[test]
    fn seed_copies_only_existing_files() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"only":"this"}"#);

        let feature_wt = project.join("login");
        std::fs::create_dir_all(&feature_wt).unwrap();
        seed_feature_claude(&project, &feature_wt).unwrap();

        let dst = feature_wt.join(".claude");
        assert!(dst.join("settings.json").exists());
        assert!(!dst.join("settings.local.json").exists());
    }

    // --- feat_new integration ---

    #[test]
    fn feat_new_copies_claude_settings_to_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"seeded":true}"#);

        crate::commands::feat_new::feat_new(
            &project,
            "login",
            None,
            None,
            None,
            false,
            server.name(),
        )
        .unwrap();

        let feat_settings = project.join("login").join(".claude").join("settings.json");
        assert!(feat_settings.exists());
        assert_eq!(
            std::fs::read_to_string(&feat_settings).unwrap(),
            r#"{"seeded":true}"#
        );
    }

    // --- push (feature → main) ---

    #[test]
    fn push_copies_feature_to_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"pushed":true}"#);
        write_json(
            &feat_claude,
            "settings.local.json",
            r#"{"local_pushed":true}"#,
        );

        push(&project, "login").unwrap();

        let main_claude = project.join("main").join(".claude");
        assert_eq!(
            std::fs::read_to_string(main_claude.join("settings.json")).unwrap(),
            r#"{"pushed":true}"#
        );
        assert_eq!(
            std::fs::read_to_string(main_claude.join("settings.local.json")).unwrap(),
            r#"{"local_pushed":true}"#
        );
    }

    #[test]
    fn push_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let result = push(&project, "nonexistent");
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn push_fails_when_feature_has_no_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        // Ensure no .claude/ dir in feature
        let feat_claude = project.join("login").join(".claude");
        if feat_claude.exists() {
            std::fs::remove_dir_all(&feat_claude).unwrap();
        }

        let result = push(&project, "login");
        assert!(result.is_err());
    }

    // --- pull (main → feature) ---

    #[test]
    fn pull_copies_main_to_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"pulled":true}"#);

        pull(&project, "login").unwrap();

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
        let (project, _, _) = server.setup_project(dir.path());

        let result = pull(&project, "nonexistent");
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn pull_fails_when_main_has_no_claude_dir() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let result = pull(&project, "login");
        assert!(result.is_err());
    }

    // --- diff ---

    /// Join diff output into a single string for assertions (strips ANSI codes).
    fn diff_output(project: &Path, feature: &str) -> String {
        let lines = diff(project, feature).unwrap();
        let joined = lines.join("\n");
        // Strip ANSI escape sequences for easier assertions
        let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(&joined, "").to_string()
    }

    #[test]
    fn diff_no_differences() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"same":true}"#);
        write_json(&feat_claude, "settings.json", r#"{"same":true}"#);

        let result = diff(&project, "login").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn diff_detects_value_difference() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
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
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
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
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
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
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"x":1}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("only in main"));
    }

    #[test]
    fn diff_file_only_in_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"x":1}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("only in feature"));
    }

    #[test]
    fn diff_both_files_missing_no_output() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let result = diff(&project, "login").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn diff_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let result = diff(&project, "nonexistent");
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    // --- merge ---

    fn read_merged(project: &Path, filename: &str) -> serde_json::Value {
        let main_claude = project.join("main").join(".claude");
        let content = std::fs::read_to_string(main_claude.join(filename)).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn merge_unions_object_keys() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"a":1}"#);
        write_json(&feat_claude, "settings.json", r#"{"b":2}"#);

        merge(&project, "login", true).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["a"], 1);
        assert_eq!(result["b"], 2);
    }

    #[test]
    fn merge_unions_arrays() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
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

        merge(&project, "login", true).unwrap();

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
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"mode":"strict"}"#);
        write_json(&feat_claude, "settings.json", r#"{"mode":"relaxed"}"#);

        // ours=false is the default (theirs/main wins)
        merge(&project, "login", false).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["mode"], "strict");
    }

    #[test]
    fn merge_ours_feature_wins_on_scalar_conflict() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"mode":"strict"}"#);
        write_json(&feat_claude, "settings.json", r#"{"mode":"relaxed"}"#);

        merge(&project, "login", true).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["mode"], "relaxed");
    }

    #[test]
    fn merge_only_feature_exists_copies_to_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"new":true}"#);

        merge(&project, "login", false).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["new"], true);
    }

    #[test]
    fn merge_only_main_exists_keeps_main() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"existing":true}"#);

        merge(&project, "login", false).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["existing"], true);
    }

    #[test]
    fn merge_neither_exists_is_noop() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        merge(&project, "login", false).unwrap();

        let main_claude = project.join("main").join(".claude");
        assert!(!main_claude.join("settings.json").exists());
    }

    #[test]
    fn merge_identical_files_is_noop() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"same":true}"#);
        write_json(&feat_claude, "settings.json", r#"{"same":true}"#);

        let before = std::fs::metadata(main_claude.join("settings.json"))
            .unwrap()
            .modified()
            .unwrap();

        merge(&project, "login", false).unwrap();

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
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
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

        merge(&project, "login", false).unwrap();

        let result = read_merged(&project, "settings.json");
        assert_eq!(result["outer"]["m_key"], "m_val");
        assert_eq!(result["outer"]["f_key"], "f_val");
    }

    #[test]
    fn merge_fails_for_nonexistent_feature() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _, _) = server.setup_project(dir.path());

        let result = merge(&project, "nonexistent", false);
        assert!(matches!(result.unwrap_err(), PmError::FeatureNotFound(_)));
    }

    #[test]
    fn merge_malformed_json_winner_takes_all() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.json", "not json");
        write_json(&feat_claude, "settings.json", r#"{"valid":true}"#);

        // Default (ours=false) → main wins
        merge(&project, "login", false).unwrap();
        let content = std::fs::read_to_string(main_claude.join("settings.json")).unwrap();
        assert_eq!(content, "not json");

        // ours=true → feature wins
        merge(&project, "login", true).unwrap();
        let content = std::fs::read_to_string(main_claude.join("settings.json")).unwrap();
        assert_eq!(content, r#"{"valid":true}"#);
    }

    #[test]
    fn diff_reports_settings_local_json_independently() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        // settings.json identical, settings.local.json differs
        write_json(&main_claude, "settings.json", r#"{"same":true}"#);
        write_json(&feat_claude, "settings.json", r#"{"same":true}"#);
        write_json(&main_claude, "settings.local.json", r#"{"env":"prod"}"#);
        write_json(&feat_claude, "settings.local.json", r#"{"env":"dev"}"#);

        let output = diff_output(&project, "login");
        assert!(output.contains("settings.local.json"));
        assert!(output.contains("env"));
    }

    #[test]
    fn merge_handles_settings_local_json_independently() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        let main_claude = project.join("main").join(".claude");
        let feat_claude = project.join("login").join(".claude");
        write_json(&main_claude, "settings.local.json", r#"{"a":1}"#);
        write_json(&feat_claude, "settings.local.json", r#"{"b":2}"#);

        merge(&project, "login", false).unwrap();

        let content = std::fs::read_to_string(main_claude.join("settings.local.json")).unwrap();
        let result: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(result["a"], 1);
        assert_eq!(result["b"], 2);
    }

    #[test]
    fn push_overwrites_main_with_feature_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        // Main has old settings
        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"old":true}"#);

        // Feature has new settings
        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"new":true}"#);

        push(&project, "login").unwrap();

        let content = std::fs::read_to_string(main_claude.join("settings.json")).unwrap();
        assert_eq!(content, r#"{"new":true}"#);
    }

    #[test]
    fn pull_overwrites_feature_with_main_settings() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, _) = server.setup_project_with_feature(dir.path(), "login");

        // Feature has diverged settings
        let feat_claude = project.join("login").join(".claude");
        write_json(&feat_claude, "settings.json", r#"{"diverged":true}"#);

        // Main has canonical settings
        let main_claude = project.join("main").join(".claude");
        write_json(&main_claude, "settings.json", r#"{"canonical":true}"#);

        pull(&project, "login").unwrap();

        let content = std::fs::read_to_string(feat_claude.join("settings.json")).unwrap();
        assert_eq!(content, r#"{"canonical":true}"#);
    }
}
