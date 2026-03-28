use std::path::{Path, PathBuf};

use crate::error::Result;

/// Convert an absolute path to a Claude Code path key.
/// `/Users/foo/bar` becomes `-Users-foo-bar`.
fn path_to_key(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.strip_suffix('/').unwrap_or(&s);
    if s.is_empty() {
        return "-".to_string();
    }
    s.replace('/', "-")
}

/// Return the default Claude base directory (`~/.claude/`).
fn claude_base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not determine home directory",
        )
    })?;
    Ok(home.join(".claude"))
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Replace all occurrences of `old_path` with `new_path` in a JSONL file, line by line.
fn update_jsonl_file(path: &Path, old_path: &str, new_path: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let updated = content.replace(old_path, new_path);
    if updated != content {
        std::fs::write(path, updated)?;
    }
    Ok(())
}

/// Update `sessions-index.json`: parse as JSON, replace path strings in all string values.
fn update_sessions_index(path: &Path, old_path: &str, new_path: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    replace_json_strings(&mut value, old_path, new_path);
    let updated = serde_json::to_string_pretty(&value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(path, updated)?;
    Ok(())
}

/// Recursively replace `old` with `new` in all JSON string values.
fn replace_json_strings(value: &mut serde_json::Value, old: &str, new: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains(old) {
                *s = s.replace(old, new);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                replace_json_strings(item, old, new);
            }
        }
        serde_json::Value::Object(obj) => {
            for (_, v) in obj.iter_mut() {
                replace_json_strings(v, old, new);
            }
        }
        _ => {}
    }
}

/// Update the global `history.jsonl` file, replacing old path in `project` field only.
/// Uses JSON-aware replacement to avoid corrupting unrelated entries.
fn update_history(path: &Path, old_path: &str, new_path: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(path)?;
    let mut changed = false;
    let updated: String = content
        .lines()
        .map(|line| {
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(project) = value.get("project").and_then(|v| v.as_str())
                && project == old_path
            {
                value["project"] = serde_json::Value::String(new_path.to_string());
                changed = true;
                return serde_json::to_string(&value).unwrap_or_else(|_| line.to_string());
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if changed {
        // Preserve trailing newline if original had one
        let final_content = if content.ends_with('\n') && !updated.ends_with('\n') {
            updated + "\n"
        } else {
            updated
        };
        std::fs::write(path, final_content)?;
    }
    Ok(())
}

/// Migrate Claude Code session data from `old_path` to `new_path`.
///
/// Copies the session directory (preserving the original) and updates all
/// embedded path references. The `claude_base` parameter allows tests to
/// use a temp directory instead of `~/.claude/`.
///
/// Returns human-readable status messages.
pub fn migrate_sessions(
    old_path: &Path,
    new_path: &Path,
    claude_base: Option<&Path>,
) -> Result<Vec<String>> {
    let base = match claude_base {
        Some(b) => b.to_path_buf(),
        None => claude_base_dir()?,
    };
    let projects_dir = base.join("projects");

    let old_key = path_to_key(old_path);
    let new_key = path_to_key(new_path);
    let old_dir = projects_dir.join(&old_key);
    let new_dir = projects_dir.join(&new_key);

    let mut messages = Vec::new();

    if !old_dir.exists() {
        messages.push(format!(
            "No Claude sessions found for {}",
            old_path.display()
        ));
        return Ok(messages);
    }

    // Copy session directory if new one doesn't exist yet
    if new_dir.exists() {
        messages.push(format!(
            "Claude session directory already exists for {}, updating path references",
            new_path.display()
        ));
    } else {
        copy_dir_recursive(&old_dir, &new_dir)?;
        messages.push(format!(
            "Copied Claude sessions from {} to {}",
            old_path.display(),
            new_path.display()
        ));
    }

    // Update path references in all files under the new directory
    let old_path_str = old_path.to_string_lossy();
    let new_path_str = new_path.to_string_lossy();

    // Update .jsonl session files (may be at top level or in subdirs)
    update_jsonl_files_recursive(&new_dir, &old_path_str, &new_path_str)?;

    // Update sessions-index.json
    let index_path = new_dir.join("sessions-index.json");
    if index_path.exists() {
        update_sessions_index(&index_path, &old_path_str, &new_path_str)?;
    }

    // Update global history
    let history_path = base.join("history.jsonl");
    update_history(&history_path, &old_path_str, &new_path_str)?;

    messages.push("Updated path references in session files".to_string());
    Ok(messages)
}

/// Walk a directory recursively and update all `.jsonl` files.
fn update_jsonl_files_recursive(dir: &Path, old_path: &str, new_path: &str) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            update_jsonl_files_recursive(&path, old_path, new_path)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            update_jsonl_file(&path, old_path, new_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn path_to_key_converts_slashes_to_dashes() {
        let key = path_to_key(Path::new("/Users/foo/bar"));
        assert_eq!(key, "-Users-foo-bar");
    }

    #[test]
    fn path_to_key_handles_root() {
        let key = path_to_key(Path::new("/"));
        assert_eq!(key, "-");
    }

    #[test]
    fn path_to_key_strips_trailing_slash() {
        let key = path_to_key(Path::new("/Users/foo/bar/"));
        assert_eq!(key, "-Users-foo-bar");
    }

    #[test]
    fn migrate_copies_session_directory() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("abc123.jsonl"), "{\"cwd\":\"/old/path\"}\n").unwrap();

        let msgs = migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        let new_dir = projects.join("-new-path");
        assert!(new_dir.exists());
        assert!(new_dir.join("abc123.jsonl").exists());
        assert!(msgs.iter().any(|m| m.contains("Copied")));
    }

    #[test]
    fn migrate_preserves_old_directory() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("session.jsonl"), "{}").unwrap();

        migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        assert!(old_dir.exists());
        assert!(old_dir.join("session.jsonl").exists());
    }

    #[test]
    fn migrate_updates_jsonl_paths() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(
            old_dir.join("session.jsonl"),
            "{\"cwd\":\"/old/path\",\"projectPath\":\"/old/path\"}\n\
             {\"cwd\":\"/old/path/sub\",\"other\":\"unrelated\"}\n",
        )
        .unwrap();

        migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        let new_dir = projects.join("-new-path");
        let content = std::fs::read_to_string(new_dir.join("session.jsonl")).unwrap();
        assert!(content.contains("/new/path"));
        assert!(!content.contains("/old/path"));
        assert!(content.contains("unrelated"));
    }

    #[test]
    fn migrate_updates_sessions_index() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        let index = serde_json::json!([{
            "sessionId": "abc",
            "fullPath": "/old/path",
            "projectPath": "/old/path"
        }]);
        std::fs::write(
            old_dir.join("sessions-index.json"),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();

        migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        let new_dir = projects.join("-new-path");
        let content = std::fs::read_to_string(new_dir.join("sessions-index.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed[0]["fullPath"], "/new/path");
        assert_eq!(parsed[0]["projectPath"], "/new/path");
    }

    #[test]
    fn migrate_updates_global_history() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("session.jsonl"), "{}").unwrap();

        // Create global history
        std::fs::write(
            base.path().join("history.jsonl"),
            "{\"project\":\"/old/path\",\"display\":\"hello\"}\n\
             {\"project\":\"/other/path\",\"display\":\"bye\"}\n",
        )
        .unwrap();

        migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        let content = std::fs::read_to_string(base.path().join("history.jsonl")).unwrap();
        assert!(content.contains("/new/path"));
        assert!(!content.contains("/old/path"));
        assert!(content.contains("/other/path"));
    }

    #[test]
    fn migrate_no_old_sessions_is_noop() {
        let base = tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("projects")).unwrap();

        let msgs = migrate_sessions(
            Path::new("/nonexistent/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        assert!(msgs.iter().any(|m| m.contains("No Claude sessions")));
        assert!(!base.path().join("projects").join("-new-path").exists());
    }

    #[test]
    fn migrate_existing_new_dir_skips_copy_but_updates() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        let new_dir = projects.join("-new-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();

        // Old has a file the new doesn't — it should NOT be copied
        std::fs::write(old_dir.join("old-only.jsonl"), "{}").unwrap();

        // New already has a session file with old paths that needs updating
        std::fs::write(new_dir.join("session.jsonl"), "{\"cwd\":\"/old/path\"}\n").unwrap();

        let msgs = migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        assert!(msgs.iter().any(|m| m.contains("already exists")));
        assert!(!new_dir.join("old-only.jsonl").exists());
        let content = std::fs::read_to_string(new_dir.join("session.jsonl")).unwrap();
        assert!(content.contains("/new/path"));
    }

    #[test]
    fn migrate_handles_nested_subdirectories() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        let subagent_dir = old_dir.join("abc123").join("subagents");
        std::fs::create_dir_all(&subagent_dir).unwrap();
        std::fs::write(
            subagent_dir.join("agent-1.jsonl"),
            "{\"cwd\":\"/old/path\"}\n",
        )
        .unwrap();
        std::fs::write(
            subagent_dir.join("agent-1.meta.json"),
            "{\"agentType\":\"Explore\"}",
        )
        .unwrap();

        // Also create memory directory
        let memory_dir = old_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("MEMORY.md"), "# Memory\n").unwrap();

        migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        let new_dir = projects.join("-new-path");
        // Subagent files copied and updated
        let agent_content = std::fs::read_to_string(
            new_dir
                .join("abc123")
                .join("subagents")
                .join("agent-1.jsonl"),
        )
        .unwrap();
        assert!(agent_content.contains("/new/path"));
        // Meta file preserved
        assert!(
            new_dir
                .join("abc123")
                .join("subagents")
                .join("agent-1.meta.json")
                .exists()
        );
        // Memory files copied
        assert!(new_dir.join("memory").join("MEMORY.md").exists());
    }

    #[test]
    fn migrate_handles_missing_history_file() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("session.jsonl"), "{}").unwrap();

        // No history.jsonl — should not error
        let result = migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn history_update_does_not_corrupt_similar_paths() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("session.jsonl"), "{}").unwrap();

        // History has entries for /old/path AND /old/path-2
        std::fs::write(
            base.path().join("history.jsonl"),
            "{\"project\":\"/old/path\",\"display\":\"hello\"}\n\
             {\"project\":\"/old/path-2\",\"display\":\"other\"}\n",
        )
        .unwrap();

        migrate_sessions(
            Path::new("/old/path"),
            Path::new("/new/path"),
            Some(base.path()),
        )
        .unwrap();

        let content = std::fs::read_to_string(base.path().join("history.jsonl")).unwrap();
        // /old/path entry should be updated
        assert!(content.contains("\"project\":\"/new/path\""));
        // /old/path-2 should NOT be touched
        assert!(content.contains("\"project\":\"/old/path-2\""));
    }
}
