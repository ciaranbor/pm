//! Claude Code's directory trust, recorded per path in the global config
//! (`.claude.json`) as `projects."<dir>".hasTrustDialogAccepted`. Since
//! 2.1.326 an `--add-dir` directory inside a git repository not yet trusted
//! stops the session at startup on a trust prompt ("Adding a new
//! directory"), whose answer lasts only for that session. An entry for the
//! exact directory, symlinked or canonical, skips it without trusting the
//! rest of its repository (verified on 2.1.326), so pm records one for each
//! of its own state directories an agent is launched with.
//!
//! Claude Code rewrites the file as it runs; a running session's write
//! landing between pm's read and write drops pm's entry, and the next
//! spawn asks again.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::Result;
use crate::fs_utils::write_private;

const GLOBAL_CONFIG: &str = ".claude.json";
const ACCEPTED: &str = "hasTrustDialogAccepted";

/// `.claude.json` in `$CLAUDE_CONFIG_DIR`, else in `home`. Tests always
/// get the latter so a developer's own config dir never leaks into a test
/// home.
pub(in crate::harness) fn global_config(home: &Path) -> PathBuf {
    #[cfg(not(test))]
    if let Some(dir) = std::env::var_os(super::input::CONFIG_DIR_ENV)
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join(GLOBAL_CONFIG);
    }
    home.join(GLOBAL_CONFIG)
}

/// Mark each of `dirs` trusted in `config`, preserving everything else in
/// it. Returns whether anything was written; a file that is not a JSON
/// object is left alone, as an error.
pub(in crate::harness) fn trust_dirs(config: &Path, dirs: &[PathBuf]) -> Result<bool> {
    let text = match std::fs::read_to_string(config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".to_string(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: Value = serde_json::from_str(&text)?;
    let Some(root) = doc.as_object_mut() else {
        return Err(not_an_object(config));
    };
    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(projects) = projects.as_object_mut() else {
        return Err(not_an_object(config));
    };
    let mut changed = false;
    for dir in dirs {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let entry = projects
            .entry(key.to_string_lossy())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(entry) = entry.as_object_mut() else {
            continue;
        };
        if entry.get(ACCEPTED) != Some(&Value::Bool(true)) {
            entry.insert(ACCEPTED.to_string(), Value::Bool(true));
            changed = true;
        }
    }
    if changed {
        write_private(config, serde_json::to_string_pretty(&doc)?.as_bytes())?;
    }
    Ok(changed)
}

fn not_an_object(config: &Path) -> crate::error::PmError {
    std::io::Error::other(format!(
        "{} is not a JSON object with a `projects` object",
        config.display()
    ))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_dirs_adds_entries_and_preserves_the_rest_of_the_file() {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = home.path().join(GLOBAL_CONFIG);
        std::fs::write(
            &config,
            r#"{"theme":"dark","projects":{"/elsewhere":{"hasTrustDialogAccepted":false,"lastCost":3}}}"#,
        )
        .unwrap();
        let summaries = dir.path().join("summaries");
        std::fs::create_dir(&summaries).unwrap();
        let elsewhere = PathBuf::from("/elsewhere");

        assert!(trust_dirs(&config, &[summaries.clone(), elsewhere.clone()]).unwrap());
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        let canonical = summaries.canonicalize().unwrap();
        assert_eq!(doc["projects"][canonical.to_str().unwrap()][ACCEPTED], true);
        assert_eq!(doc["projects"]["/elsewhere"][ACCEPTED], true);
        assert_eq!(doc["projects"]["/elsewhere"]["lastCost"], 3);
        assert_eq!(doc["theme"], "dark");

        assert!(!trust_dirs(&config, &[summaries, elsewhere]).unwrap());
    }

    #[test]
    fn trust_dirs_creates_a_missing_config_and_refuses_a_malformed_one() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(GLOBAL_CONFIG);
        assert!(trust_dirs(&config, &[PathBuf::from("/d")]).unwrap());
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(doc["projects"]["/d"][ACCEPTED], true);

        std::fs::write(&config, "[1]").unwrap();
        assert!(trust_dirs(&config, &[PathBuf::from("/d")]).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "[1]");
    }
}
