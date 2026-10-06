//! Claude Code keeps a directory's sessions in
//! `~/.claude/projects/<key>/`, the key being the directory's absolute path
//! with every character but ASCII letters and digits replaced by `-`, and
//! records that path inside the transcripts. A session is `<id>.jsonl` plus,
//! when it ran subagents, a directory `<id>/`.
//!
//! Moving sessions is a copy to the new key plus a rewrite of the embedded
//! paths; session ids are file names and survive it. Only what the new key
//! lacks is copied, and only what was copied is rewritten, so migrating
//! again carries what an earlier run left behind and touches nothing twice.
//! A running Claude Code keeps appending under the key it started with, so
//! a session in use is left for a later run: a copy taken now would be
//! resumed without the turns that follow it. The path Claude Code records
//! is the resolved one, so a directory moved to a symlink of itself has
//! nothing to carry.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fs_utils::copy_dir_recursive;
use crate::harness::{ImportOutcome, InUse, session_counts};

const INDEX_FILE: &str = "sessions-index.json";
const MEMORY_DIR: &str = "memory";

/// The store key of the directory at `path`: `/Users/foo/my_app` becomes
/// `-Users-foo-my-app`.
pub(crate) fn path_to_key(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.strip_suffix('/').unwrap_or(&s);
    if s.is_empty() {
        return "-".to_string();
    }
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn update_jsonl_file(path: &Path, old_path: &str, new_path: &str) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let updated = content.replace(old_path, new_path);
    if updated != content {
        std::fs::write(path, updated)?;
    }
    Ok(())
}

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

fn replace_json_strings(value: &mut serde_json::Value, old: &str, new: &str) {
    match value {
        serde_json::Value::String(s) if s.contains(old) => {
            *s = s.replace(old, new);
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

/// Point the entries of the global `history.jsonl` whose `project` is
/// exactly `old_path` at `new_path`.
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
        let final_content = if content.ends_with('\n') && !updated.ends_with('\n') {
            updated + "\n"
        } else {
            updated
        };
        std::fs::write(path, final_content)?;
    }
    Ok(())
}

/// Rewrite `old_path` to `new_path` in what was copied to `target`.
fn rewrite_paths(target: &Path, old_path: &str, new_path: &str) -> Result<()> {
    if target.is_dir() {
        for entry in std::fs::read_dir(target)? {
            rewrite_paths(&entry?.path(), old_path, new_path)?;
        }
    } else if target.file_name().is_some_and(|name| name == INDEX_FILE) {
        update_sessions_index(target, old_path, new_path)?;
    } else if target.extension().is_some_and(|ext| ext == "jsonl") {
        update_jsonl_file(target, old_path, new_path)?;
    }
    Ok(())
}

/// `dir` as Claude Code records it: resolved. One that is gone resolves
/// through its parent.
fn recorded(dir: &Path) -> PathBuf {
    dir.canonicalize()
        .ok()
        .or_else(|| Some(dir.parent()?.canonicalize().ok()?.join(dir.file_name()?)))
        .unwrap_or_else(|| dir.to_path_buf())
}

/// Carry the sessions recorded at `old_path` over to `new_path` in the store
/// at `base` (`~/.claude`), leaving the originals in place. Returns
/// human-readable status messages.
pub(crate) fn migrate_sessions(
    base: &Path,
    old_path: &Path,
    new_path: &Path,
    in_use: &[InUse],
) -> Result<Vec<String>> {
    let target = recorded(new_path);
    let mut sources = vec![old_path.to_path_buf()];
    if !sources.contains(&recorded(old_path)) {
        sources.push(recorded(old_path));
    }
    // `new_path` reached through a symlink to `old_path`.
    if sources.contains(&target) {
        return Ok(vec![format!(
            "Claude sessions of {} are recorded at the directory {} resolves to",
            old_path.display(),
            new_path.display()
        )]);
    }

    let projects_dir = base.join("projects");
    let new_dir = projects_dir.join(path_to_key(&target));
    let new_path_str = target.to_string_lossy();

    let mut messages = Vec::new();
    let mut left = Vec::new();
    let mut found = false;
    let mut copied = 0;
    for source in &sources {
        let old_dir = projects_dir.join(path_to_key(source));
        if !old_dir.exists() || old_dir == new_dir {
            continue;
        }
        found = true;
        let old_path_str = source.to_string_lossy();

        let mut names: Vec<_> = std::fs::read_dir(&old_dir)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()?;
        names.sort();
        for name in names {
            let text = name.to_string_lossy();
            let transcript = text.strip_suffix(".jsonl");
            let session = transcript.unwrap_or(&text);
            if let Some(holder) = in_use.iter().find(|u| u.session_id == session) {
                if !left.contains(&session.to_string()) {
                    left.push(session.to_string());
                    messages.push(format!(
                        "Left session {session} at {}: agent {} is running on it; stop the \
                         agent and migrate again",
                        old_path.display(),
                        holder.agent
                    ));
                }
                continue;
            }

            let from = old_dir.join(&name);
            let to = new_dir.join(&name);
            if to.exists() {
                continue;
            }
            if from.is_dir() {
                copy_dir_recursive(&from, &to)?;
            } else {
                std::fs::create_dir_all(&new_dir)?;
                std::fs::copy(&from, &to)?;
            }
            rewrite_paths(&to, &old_path_str, &new_path_str)?;
            if transcript.is_some() {
                copied += 1;
            }
        }
        update_history(&base.join("history.jsonl"), &old_path_str, &new_path_str)?;
    }

    if !found {
        return Ok(vec![format!(
            "No Claude sessions found for {}",
            old_path.display()
        )]);
    }
    if copied > 0 {
        messages.push(format!(
            "Copied {copied} Claude session(s) from {} to {}",
            old_path.display(),
            new_path.display()
        ));
    } else if left.is_empty() {
        messages.push(format!(
            "No Claude sessions of {} left to copy to {}",
            old_path.display(),
            new_path.display()
        ));
    }
    Ok(messages)
}

/// Whether any session is recorded at `dir`.
pub(crate) fn has_sessions(base: &Path, dir: &Path) -> bool {
    [dir.to_path_buf(), recorded(dir)]
        .iter()
        .any(|form| base.join("projects").join(path_to_key(form)).exists())
}

/// Copy the sessions recorded at `dir` into `staging`. Returns the store key
/// they were found under, or `None` when there are none.
pub(crate) fn export(base: &Path, dir: &Path, staging: &Path) -> Result<Option<String>> {
    let projects = base.join("projects");
    let Some(key) = [recorded(dir), dir.to_path_buf()]
        .iter()
        .map(|form| path_to_key(form))
        .find(|key| projects.join(key).exists())
    else {
        return Ok(None);
    };
    let src = projects.join(&key);
    copy_dir_recursive(&src, staging)?;
    Ok(Some(key))
}

/// What [`merge`] wrote, and what it found already there.
#[derive(Default)]
struct Merged {
    /// Every file written, for the path rewrite.
    copied: Vec<PathBuf>,
    sessions: usize,
    sessions_present: usize,
    memory: usize,
    /// Memory files and session indexes the target already had with other
    /// contents.
    kept: Vec<String>,
}

/// Copy into `target` every file under `staging` that `target` lacks,
/// recording at `rel` below the store key.
fn merge(staging: &Path, target: &Path, rel: &Path, out: &mut Merged) -> Result<()> {
    let mut names: Vec<_> = std::fs::read_dir(staging)?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<std::io::Result<_>>()?;
    names.sort();
    let top = rel.as_os_str().is_empty();
    let in_memory = rel.starts_with(MEMORY_DIR);
    for name in names {
        let (from, to, rel) = (staging.join(&name), target.join(&name), rel.join(&name));
        if from.is_dir() {
            merge(&from, &to, &rel, out)?;
            continue;
        }
        let session = top && to.extension().is_some_and(|ext| ext == "jsonl");
        if to.exists() {
            if session {
                out.sessions_present += 1;
            } else if (in_memory || (top && name == INDEX_FILE))
                && std::fs::read(&from)? != std::fs::read(&to)?
            {
                out.kept.push(rel.to_string_lossy().into_owned());
            }
            continue;
        }
        std::fs::create_dir_all(target)?;
        std::fs::copy(&from, &to)?;
        out.copied.push(to);
        if session {
            out.sessions += 1;
        } else if in_memory {
            out.memory += 1;
        }
    }
    Ok(())
}

/// Install the sessions in `staging`, exported from `from`, as the sessions
/// of `to`: every session, subagent transcript and memory file the store
/// lacks. A file the store already has is kept as it is. `from` is another
/// machine's path: it is what the transcripts hold, and names nothing here.
pub(crate) fn import(base: &Path, staging: &Path, from: &Path, to: &Path) -> Result<ImportOutcome> {
    let to = &recorded(to);
    let target = base.join("projects").join(path_to_key(to));
    let (old, new) = (from.to_string_lossy(), to.to_string_lossy());
    let mut merged = Merged::default();
    merge(staging, &target, Path::new(""), &mut merged)?;
    if from != to {
        for file in &merged.copied {
            rewrite_paths(file, &old, &new)?;
        }
    }

    let notes: Vec<String> = merged
        .kept
        .iter()
        .map(|file| format!("kept the local {file}; the exported one differs"))
        .collect();
    let counts = session_counts(merged.sessions, merged.sessions + merged.sessions_present);
    if merged.copied.is_empty() {
        let why = counts.err().unwrap_or_default();
        return Ok(ImportOutcome::Skipped(
            std::iter::once(why)
                .chain(notes)
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let mut detail = vec![counts.unwrap_or_else(|why| why)];
    if merged.memory > 0 {
        detail.push(format!("{} memory file(s)", merged.memory));
    }
    if from != to {
        detail.push("path rewritten".to_string());
    }
    Ok(ImportOutcome::Imported {
        detail: detail.join(", "),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn migrate(base: &Path) -> Vec<String> {
        migrate_sessions(base, Path::new("/old/path"), Path::new("/new/path"), &[]).unwrap()
    }

    #[test]
    fn path_to_key_replaces_everything_but_letters_and_digits() {
        assert_eq!(path_to_key(Path::new("/Users/foo/bar")), "-Users-foo-bar");
        // As Claude Code keys `/var/folders/j_/x.y/my-app`.
        assert_eq!(
            path_to_key(Path::new("/var/folders/j_/x.y/my-app")),
            "-var-folders-j--x-y-my-app"
        );
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

        let msgs = migrate(base.path());

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

        migrate(base.path());

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

        migrate(base.path());

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

        migrate(base.path());

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

        migrate(base.path());

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
            base.path(),
            Path::new("/nonexistent/path"),
            Path::new("/new/path"),
            &[],
        )
        .unwrap();

        assert!(msgs.iter().any(|m| m.contains("No Claude sessions")));
        assert!(!base.path().join("projects").join("-new-path").exists());
    }

    #[test]
    fn migrate_adds_what_the_new_directory_lacks_and_rewrites_only_that() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        let new_dir = projects.join("-old-path2");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(old_dir.join("carried.jsonl"), "{\"cwd\":\"/old/path\"}\n").unwrap();
        std::fs::write(old_dir.join("both.jsonl"), "{\"cwd\":\"/old/path\"}\n").unwrap();
        // Recorded at the new path, which contains the old one.
        std::fs::write(new_dir.join("both.jsonl"), "{\"cwd\":\"/old/path2\"}\n").unwrap();

        let run = || {
            migrate_sessions(
                base.path(),
                Path::new("/old/path"),
                Path::new("/old/path2"),
                &[],
            )
            .unwrap()
        };
        assert_eq!(
            run(),
            ["Copied 1 Claude session(s) from /old/path to /old/path2"]
        );
        assert_eq!(
            run(),
            ["No Claude sessions of /old/path left to copy to /old/path2"]
        );

        for file in ["carried.jsonl", "both.jsonl"] {
            assert_eq!(
                std::fs::read_to_string(new_dir.join(file)).unwrap(),
                "{\"cwd\":\"/old/path2\"}\n",
                "{file}"
            );
        }
    }

    #[test]
    fn migrate_to_a_symlink_of_the_old_directory_changes_nothing() {
        let base = tempdir().unwrap();
        let work = tempdir().unwrap();
        let repo = work.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let link = work.path().join("main");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let store = base.path().join("projects").join(path_to_key(&repo));
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("abc.jsonl"), "{}\n").unwrap();
        let history = format!("{{\"project\":\"{}\"}}\n", repo.display());
        std::fs::write(base.path().join("history.jsonl"), &history).unwrap();

        let messages = migrate_sessions(base.path(), &repo, &link, &[]).unwrap();

        assert!(
            messages[0].contains("are recorded at the directory"),
            "{messages:?}"
        );
        assert_eq!(
            std::fs::read_to_string(base.path().join("history.jsonl")).unwrap(),
            history
        );
        assert_eq!(
            std::fs::read_dir(base.path().join("projects"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn migrate_keys_the_new_directory_by_its_resolved_path() {
        let base = tempdir().unwrap();
        let work = tempdir().unwrap();
        let real = work.path().canonicalize().unwrap().join("real");
        std::fs::create_dir_all(real.join("auth")).unwrap();
        let link = work.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // Recorded while the worktree was `login`, which is gone.
        let store = base
            .path()
            .join("projects")
            .join(path_to_key(&real.join("login")));
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("abc.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", real.join("login").display()),
        )
        .unwrap();

        migrate_sessions(base.path(), &link.join("login"), &link.join("auth"), &[]).unwrap();

        let carried = base
            .path()
            .join("projects")
            .join(path_to_key(&real.join("auth")));
        assert_eq!(
            std::fs::read_to_string(carried.join("abc.jsonl")).unwrap(),
            format!("{{\"cwd\":\"{}\"}}\n", real.join("auth").display())
        );
    }

    #[test]
    fn import_treats_the_exported_path_as_text_even_when_it_exists_here() {
        let base = tempdir().unwrap();
        let work = tempdir().unwrap();
        let work_dir = work.path().canonicalize().unwrap();
        // The exported path is, on this machine, a symlink to a directory
        // with sessions of its own.
        let local = work_dir.join("local");
        let to = work_dir.join("main");
        for dir in [&local, &to] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let from = work_dir.join("exported");
        std::os::unix::fs::symlink(&local, &from).unwrap();
        let local_store = base.path().join("projects").join(path_to_key(&local));
        std::fs::create_dir_all(&local_store).unwrap();
        let local_transcript = format!("{{\"cwd\":\"{}\"}}\n", local.display());
        std::fs::write(local_store.join("mine.jsonl"), &local_transcript).unwrap();

        let staging = work_dir.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(
            staging.join("theirs.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", from.display()),
        )
        .unwrap();

        let outcome = import(base.path(), &staging, &from, &to).unwrap();

        assert_eq!(
            outcome,
            ImportOutcome::Imported {
                detail: "1 session(s), path rewritten".to_string(),
                notes: Vec::new(),
            }
        );
        let imported = base.path().join("projects").join(path_to_key(&to));
        assert_eq!(
            std::fs::read_to_string(imported.join("theirs.jsonl")).unwrap(),
            format!("{{\"cwd\":\"{}\"}}\n", to.display())
        );
        assert!(!imported.join("mine.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(local_store.join("mine.jsonl")).unwrap(),
            local_transcript
        );
        assert_eq!(std::fs::read_dir(&local_store).unwrap().count(), 1);
    }

    #[test]
    fn import_adds_what_the_store_lacks_and_keeps_what_it_has() {
        let base = tempdir().unwrap();
        let staging = tempdir().unwrap();
        let (from, to) = (Path::new("/there/repo"), Path::new("/here/repo"));
        let write = |dir: &Path, rel: &str, text: &str| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        let exported = "{\"cwd\":\"/there/repo\"}\n";
        for rel in [
            "kept.jsonl",
            "new.jsonl",
            "new/subagents/agent-1.jsonl",
            "kept/subagents/agent-2.jsonl",
        ] {
            write(staging.path(), rel, exported);
        }
        write(staging.path(), "memory/MEMORY.md", "theirs\n");
        write(staging.path(), "memory/idea.md", "idea\n");
        let store = base.path().join("projects").join("-here-repo");
        let local = "{\"cwd\":\"/here/repo\",\"turn\":9}\n";
        write(&store, "kept.jsonl", local);
        write(&store, "memory/MEMORY.md", "mine\n");

        let outcome = import(base.path(), staging.path(), from, to).unwrap();

        assert_eq!(
            outcome,
            ImportOutcome::Imported {
                detail: "1 session(s), 1 already present, 1 memory file(s), path rewritten"
                    .to_string(),
                notes: vec![
                    "kept the local memory/MEMORY.md; the exported one differs".to_string()
                ],
            }
        );
        let read = |rel: &str| std::fs::read_to_string(store.join(rel)).unwrap();
        assert_eq!(read("kept.jsonl"), local);
        assert_eq!(read("memory/MEMORY.md"), "mine\n");
        assert_eq!(read("memory/idea.md"), "idea\n");
        for rel in [
            "new.jsonl",
            "new/subagents/agent-1.jsonl",
            "kept/subagents/agent-2.jsonl",
        ] {
            assert_eq!(read(rel), "{\"cwd\":\"/here/repo\"}\n", "{rel}");
        }

        assert_eq!(
            import(base.path(), staging.path(), from, to).unwrap(),
            ImportOutcome::Skipped(
                "all 2 session(s) already exist locally; kept the local memory/MEMORY.md; \
                 the exported one differs"
                    .to_string()
            )
        );
        assert_eq!(read("new.jsonl"), "{\"cwd\":\"/here/repo\"}\n");
    }

    #[test]
    fn migrate_leaves_a_session_in_use_for_a_later_run() {
        let base = tempdir().unwrap();
        let projects = base.path().join("projects");
        let old_dir = projects.join("-old-path");
        let new_dir = projects.join("-new-path");
        std::fs::create_dir_all(old_dir.join("running/subagents")).unwrap();
        std::fs::write(old_dir.join("running.jsonl"), "{\"turn\":1}\n").unwrap();
        std::fs::write(old_dir.join("running/subagents/a.jsonl"), "{}\n").unwrap();
        std::fs::write(old_dir.join("idle.jsonl"), "{}\n").unwrap();
        let in_use = [InUse {
            session_id: "running".to_string(),
            agent: "login/reviewer".to_string(),
        }];
        let run = |in_use: &[InUse]| {
            migrate_sessions(
                base.path(),
                Path::new("/old/path"),
                Path::new("/new/path"),
                in_use,
            )
            .unwrap()
        };

        let messages = run(&in_use);

        assert_eq!(
            messages,
            [
                "Left session running at /old/path: agent login/reviewer is running on it; \
                 stop the agent and migrate again",
                "Copied 1 Claude session(s) from /old/path to /new/path",
            ]
        );
        assert!(new_dir.join("idle.jsonl").exists());
        assert!(!new_dir.join("running.jsonl").exists());
        assert!(!new_dir.join("running").exists());

        // The agent went on for another turn before it was stopped.
        std::fs::write(
            old_dir.join("running.jsonl"),
            "{\"turn\":1}\n{\"turn\":2}\n",
        )
        .unwrap();
        run(&[]);
        assert_eq!(
            std::fs::read_to_string(new_dir.join("running.jsonl")).unwrap(),
            "{\"turn\":1}\n{\"turn\":2}\n"
        );
        assert!(new_dir.join("running/subagents/a.jsonl").exists());
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

        migrate(base.path());

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
            base.path(),
            Path::new("/old/path"),
            Path::new("/new/path"),
            &[],
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

        migrate(base.path());

        let content = std::fs::read_to_string(base.path().join("history.jsonl")).unwrap();
        // /old/path entry should be updated
        assert!(content.contains("\"project\":\"/new/path\""));
        // /old/path-2 should NOT be touched
        assert!(content.contains("\"project\":\"/old/path-2\""));
    }
}
