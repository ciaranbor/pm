//! `pm harness import`: install the sessions of a `pm harness export`
//! tarball for every project in its manifest that is registered here, as
//! sessions of that project's local main worktree.

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::harness::{Harness, ImportOutcome, SessionStore};
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectEntry, harness_config_in};

use super::harness_export::{MANIFEST, PROJECTS_DIR, export_root};

/// The harness an export's manifest entry belongs to. Exports written
/// before other harnesses had sessions to export name none.
fn recorded_harness(info: &serde_json::Value) -> &str {
    info.get("harness")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(Harness::ClaudeCode.as_str())
}

/// The root of whichever harness's export was extracted into `staging`.
fn find_export(staging: &Path) -> Option<PathBuf> {
    Harness::SUPPORTED
        .iter()
        .map(|harness| staging.join(export_root(*harness)))
        .find(|root| root.join(MANIFEST).exists())
}

/// Import `harness`'s sessions from `tarball` into the store reached from
/// `home`. `global` is the global tier's `[harness.*]` settings.
///
/// Returns human-readable status messages.
pub fn import(
    harness: Harness,
    tarball: &Path,
    projects_dir: &Path,
    home: &Path,
    global: &HarnessConfig,
) -> Result<Vec<String>> {
    if !tarball.exists() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("tarball not found: {}", tarball.display()),
        )));
    }

    let staging = tempfile::tempdir()?;
    let status = std::process::Command::new("tar")
        .args([
            "-xzf",
            &tarball.to_string_lossy(),
            "-C",
            &staging.path().to_string_lossy(),
        ])
        .status()?;
    if !status.success() {
        return Err(PmError::ExportImport("tar extraction failed".to_string()));
    }

    let export_root = find_export(staging.path()).ok_or_else(|| {
        PmError::ExportImport("invalid export: manifest.json not found".to_string())
    })?;
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(export_root.join(MANIFEST))?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let manifest = manifest
        .as_object()
        .ok_or_else(|| PmError::ExportImport("manifest.json is not an object".to_string()))?;

    // A session only means something to the harness that recorded it.
    if let Some(other) = manifest
        .values()
        .map(recorded_harness)
        .find(|recorded| *recorded != harness.as_str())
    {
        return Err(PmError::ExportImport(format!(
            "{} holds {other} sessions, not {harness} ones; import it with `--harness {other}`",
            tarball.display()
        )));
    }

    let mut messages = Vec::new();
    for (name, info) in manifest {
        let field = |key: &str| {
            info[key].as_str().ok_or_else(|| {
                PmError::ExportImport(format!("missing '{key}' for project '{name}'"))
            })
        };
        let from = Path::new(field("path")?);
        let sessions = export_root.join(PROJECTS_DIR).join(field("key")?);

        let Ok(local) = ProjectEntry::load(projects_dir, name) else {
            messages.push(format!("Skipping '{name}': not registered locally"));
            continue;
        };
        if !sessions.exists() {
            messages.push(format!(
                "Skipping '{name}': session data not found in tarball"
            ));
            continue;
        }

        let root = local.root_path();
        let config = harness_config_in(Some(&root), global);
        let store = SessionStore {
            home,
            config: &config,
        };
        match harness.import_sessions(&store, &sessions, from, &paths::main_worktree(&root))? {
            ImportOutcome::Skipped(why) => messages.push(format!("Skipping '{name}': {why}")),
            ImportOutcome::Imported { detail, notes } => {
                messages.extend(notes.iter().map(|note| format!("  {name}: {note}")));
                messages.push(format!("Imported '{name}' ({detail})"));
            }
        }
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::harness_export::tests::{
        register, run_export, setup_claude_sessions, setup_project,
    };
    use tempfile::tempdir;

    fn run_import(
        harness: Harness,
        tarball: &Path,
        projects_dir: &Path,
        home: &Path,
    ) -> Result<Vec<String>> {
        import(
            harness,
            tarball,
            projects_dir,
            home,
            &HarnessConfig::default(),
        )
    }

    /// A machine holding Claude Code sessions for project `myapp`, and the
    /// tarball exported from it.
    struct Source {
        _home: tempfile::TempDir,
        project: tempfile::TempDir,
        _registry: tempfile::TempDir,
        _out: tempfile::TempDir,
        main: PathBuf,
        tarball: PathBuf,
    }

    fn claude_source() -> Source {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let out = tempdir().unwrap();
        let main = setup_project(project.path(), "myapp", registry.path());
        setup_claude_sessions(home.path(), &main);
        let (tarball, _) = run_export(
            Harness::ClaudeCode,
            Some(project.path()),
            registry.path(),
            &out.path().join("export.tar.gz"),
            home.path(),
        )
        .unwrap();
        Source {
            _home: home,
            project,
            _registry: registry,
            _out: out,
            main,
            tarball,
        }
    }

    fn claude_sessions_of(home: &Path, main: &Path) -> PathBuf {
        home.join(".claude/projects")
            .join(main.to_string_lossy().replace('/', "-"))
    }

    #[test]
    fn import_same_path() {
        let source = claude_source();
        let home = tempdir().unwrap();
        let registry = tempdir().unwrap();
        register(source.project.path(), "myapp", registry.path());

        let msgs = run_import(
            Harness::ClaudeCode,
            &source.tarball,
            registry.path(),
            home.path(),
        )
        .unwrap();

        assert_eq!(msgs, ["Imported 'myapp' (same path)"]);
        let imported = claude_sessions_of(home.path(), &source.main);
        assert!(imported.join("session.jsonl").exists());
    }

    #[test]
    fn import_different_path_rewrites() {
        let source = claude_source();
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let local_main = setup_project(project.path(), "myapp", registry.path());

        let msgs = run_import(
            Harness::ClaudeCode,
            &source.tarball,
            registry.path(),
            home.path(),
        )
        .unwrap();

        assert_eq!(
            msgs.last().map(String::as_str),
            Some("Imported 'myapp' (path rewritten)")
        );
        let imported = claude_sessions_of(home.path(), &local_main);
        let content = std::fs::read_to_string(imported.join("session.jsonl")).unwrap();
        assert!(content.contains(&local_main.to_string_lossy().to_string()));
        assert!(!content.contains(&source.main.to_string_lossy().to_string()));
        // The export's own key was only a staging post.
        assert!(!claude_sessions_of(home.path(), &source.main).exists());
    }

    #[test]
    fn import_skips_unregistered_projects() {
        let source = claude_source();
        let home = tempdir().unwrap();
        let registry = tempdir().unwrap();

        let msgs = run_import(
            Harness::ClaudeCode,
            &source.tarball,
            registry.path(),
            home.path(),
        )
        .unwrap();

        assert_eq!(msgs, ["Skipping 'myapp': not registered locally"]);
        assert!(!home.path().join(".claude").exists());
    }

    #[test]
    fn import_never_replaces_sessions_that_exist_locally() {
        let source = claude_source();
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let local_main = setup_project(project.path(), "myapp", registry.path());
        let existing = setup_claude_sessions(home.path(), &local_main);
        std::fs::write(existing.join("session.jsonl"), "local history\n").unwrap();

        let msgs = run_import(
            Harness::ClaudeCode,
            &source.tarball,
            registry.path(),
            home.path(),
        )
        .unwrap();

        assert_eq!(
            msgs,
            ["Skipping 'myapp': Claude sessions already exist locally"]
        );
        assert_eq!(
            std::fs::read_to_string(existing.join("session.jsonl")).unwrap(),
            "local history\n"
        );
    }

    #[test]
    fn import_nonexistent_tarball_errors() {
        let registry = tempdir().unwrap();
        let home = tempdir().unwrap();
        let result = run_import(
            Harness::ClaudeCode,
            Path::new("/nonexistent/tarball.tar.gz"),
            registry.path(),
            home.path(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn import_refuses_an_export_of_another_harness() {
        let source = claude_source();
        let home = tempdir().unwrap();
        let registry = tempdir().unwrap();
        register(source.project.path(), "myapp", registry.path());

        let err = run_import(
            Harness::Codex,
            &source.tarball,
            registry.path(),
            home.path(),
        )
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("holds claude-code sessions, not codex ones")
                && err.ends_with("`--harness claude-code`"),
            "{err}"
        );
        assert!(!home.path().join(".codex").exists());
        assert!(!home.path().join(".claude").exists());
    }

    #[test]
    fn import_reads_an_export_whose_manifest_names_no_harness() {
        // The layout pm wrote when only Claude Code sessions were exported.
        let staging = tempdir().unwrap();
        let root = staging.path().join("pm-claude-export");
        let sessions = root.join("projects/-old-machine-myapp-main");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("session.jsonl"),
            "{\"cwd\":\"/old/machine/myapp/main\"}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("manifest.json"),
            r#"{"myapp": {"path": "/old/machine/myapp/main", "key": "-old-machine-myapp-main"}}"#,
        )
        .unwrap();
        let tarball = staging.path().join("legacy.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf", &tarball.to_string_lossy(), "-C"])
            .arg(staging.path())
            .arg("pm-claude-export")
            .status()
            .unwrap();
        assert!(status.success());

        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let local_main = setup_project(project.path(), "myapp", registry.path());

        let msgs = run_import(Harness::ClaudeCode, &tarball, registry.path(), home.path()).unwrap();

        assert_eq!(
            msgs.last().map(String::as_str),
            Some("Imported 'myapp' (path rewritten)")
        );
        let content = std::fs::read_to_string(
            claude_sessions_of(home.path(), &local_main).join("session.jsonl"),
        )
        .unwrap();
        assert!(content.contains(&local_main.to_string_lossy().to_string()));

        // It is a Claude Code export to every other harness.
        let err = run_import(Harness::OpenCode, &tarball, registry.path(), home.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("holds claude-code sessions"), "{err}");
    }

    #[test]
    fn codex_sessions_round_trip_between_two_machines() {
        const ID: &str = "01a0e810-e3f9-7fa1-a479-e6eccfd79a9c";
        let rollout = format!("sessions/2026/09/28/rollout-2026-09-28T13-50-16-{ID}.jsonl");

        let source_home = tempdir().unwrap();
        let source_project = tempdir().unwrap();
        let source_registry = tempdir().unwrap();
        let out = tempdir().unwrap();
        let source_main = setup_project(source_project.path(), "myapp", source_registry.path());
        let content = format!(
            "{}\n",
            serde_json::json!({
                "type": "session_meta",
                "payload": {"id": ID, "cwd": source_main.canonicalize().unwrap()},
            })
        );
        let stored = source_home.path().join(".codex").join(&rollout);
        std::fs::create_dir_all(stored.parent().unwrap()).unwrap();
        std::fs::write(&stored, &content).unwrap();

        let (tarball, msgs) = run_export(
            Harness::Codex,
            Some(source_project.path()),
            source_registry.path(),
            &out.path().join("codex.tar.gz"),
            source_home.path(),
        )
        .unwrap();
        assert_eq!(msgs[0], "Exported 'myapp' (1 session(s))");

        // The project lives at another path on the importing machine.
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        setup_project(project.path(), "myapp", registry.path());

        let msgs = run_import(Harness::Codex, &tarball, registry.path(), home.path()).unwrap();
        assert_eq!(msgs, ["Imported 'myapp' (1 session(s))"]);
        assert_eq!(
            std::fs::read_to_string(home.path().join(".codex").join(&rollout)).unwrap(),
            content
        );

        let again = run_import(Harness::Codex, &tarball, registry.path(), home.path()).unwrap();
        assert_eq!(
            again,
            ["Skipping 'myapp': all 1 session(s) already exist locally"]
        );
    }

    #[test]
    fn opencode_sessions_round_trip_between_two_machines() {
        use crate::state::project::OpenCodeConfig;
        use crate::testing::{fake_opencode, fake_opencode_calls, fake_opencode_sequence};

        let opencode = |binary: String| HarnessConfig {
            opencode: OpenCodeConfig {
                binary: Some(binary),
                ..Default::default()
            },
            ..Default::default()
        };

        let source_project = tempdir().unwrap();
        let source_registry = tempdir().unwrap();
        let out = tempdir().unwrap();
        let source_opencode = tempdir().unwrap();
        let source_root = source_project.path().canonicalize().unwrap();
        setup_project(&source_root, "myapp", source_registry.path());
        let exported = r#"{"info":{"id":"ses_a"},"messages":[]}"#;
        let listing = r#"{"data":[{"id":"ses_a"}],"cursor":{"previous":null,"next":null}}"#;
        let (tarball, msgs) = crate::commands::harness_export::export(
            &crate::commands::harness_export::ExportParams {
                harness: Harness::OpenCode,
                project_root: Some(&source_root),
                projects_dir: source_registry.path(),
                all: false,
                output: Some(&out.path().join("opencode.tar.gz")),
                home: source_project.path(),
                global: &opencode(fake_opencode_sequence(
                    source_opencode.path(),
                    &[listing, exported],
                    0,
                )),
            },
        )
        .unwrap();
        assert_eq!(msgs[0], "Exported 'myapp' (1 session(s))");

        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let local_opencode = tempdir().unwrap();
        let local_main = setup_project(project.path(), "myapp", registry.path());
        let msgs = import(
            Harness::OpenCode,
            &tarball,
            registry.path(),
            project.path(),
            &opencode(fake_opencode(local_opencode.path(), "", 0)),
        )
        .unwrap();

        assert_eq!(msgs, ["Imported 'myapp' (1 session(s))"]);
        let calls = fake_opencode_calls(local_opencode.path());
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            calls[0][..5],
            [
                "session",
                "import",
                "--standalone",
                "--directory",
                local_main.canonicalize().unwrap().to_str().unwrap(),
            ]
        );
        // The file handed to opencode is the one the source machine exported.
        assert!(calls[0][5].ends_with("/ses_a.json"), "{calls:?}");
    }
}
