//! `pm harness import`: install the sessions of a `pm harness export`
//! tarball for every project in its manifest that is registered here, as
//! sessions of that project's local main worktree and of each exported
//! feature's local worktree. A feature that is not registered here or has
//! no worktree is skipped: `pm restore` recreates feature worktrees, so it
//! runs first.
//!
//! A tarball is untrusted input. `tar` keeps what it extracts inside the
//! staging directory (it refuses `..` members, strips a leading `/`, and
//! will not write through a link); what it does extract is refused unless
//! it is plain files and directories, so nothing read from staging can be a
//! link to a file of this machine. The manifest's strings are joined onto
//! paths only once they are known to stay where they are joined.

use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::error::{PmError, Result};
use crate::harness::{Harness, ImportOutcome, SessionStore};
use crate::state::feature::FeatureState;
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

/// Whichever harness's export was extracted into `staging`, and its root.
fn find_export(staging: &Path) -> Option<(Harness, PathBuf)> {
    Harness::SUPPORTED
        .iter()
        .map(|harness| (*harness, staging.join(export_root(*harness))))
        .find(|(_, root)| root.join(MANIFEST).exists())
}

/// Refuse an extracted tree holding anything but directories and files of
/// their own: a symlink, a hard link, a device.
fn refuse_links(staging: &Path, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            refuse_links(staging, &path)?;
        } else if !meta.is_file() || meta.nlink() > 1 {
            return Err(PmError::ExportImport(format!(
                "invalid export: '{}' is a link or special file",
                path.strip_prefix(staging).unwrap_or(&path).display()
            )));
        }
    }
    Ok(())
}

/// Whether `value` names one entry of a directory and nothing else.
fn is_single_name(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(name)) if name == value)
        && components.next().is_none()
}

/// What [`import`] did, as human-readable status messages.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub messages: Vec<String>,
    /// The exported worktrees whose sessions were not installed, each
    /// with the reason; also in `messages`.
    pub missed: Vec<String>,
}

impl ImportReport {
    fn miss(&mut self, label: &str, why: &str) {
        self.messages.push(format!("Skipping '{label}': {why}"));
        self.missed.push(format!("'{label}' ({why})"));
    }
}

/// Import the sessions of `tarball` into the store reached from `home`:
/// those of the harness that exported it, which must be `harness` when one
/// is given, for the registered `projects` (every one when empty). `global`
/// is the global tier's `[harness.*]` settings.
pub fn import(
    harness: Option<Harness>,
    tarball: &Path,
    projects: &[String],
    projects_dir: &Path,
    home: &Path,
    global: &HarnessConfig,
) -> Result<ImportReport> {
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

    refuse_links(staging.path(), staging.path())?;

    let (exported, export_root) = find_export(staging.path()).ok_or_else(|| {
        PmError::ExportImport("invalid export: manifest.json not found".to_string())
    })?;
    let harness = harness.unwrap_or(exported);
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

    // Every entry is checked before anything is imported, so a refused
    // export leaves the store untouched.
    let exported = manifest
        .iter()
        .map(|(name, info)| Ok((name.as_str(), worktrees(&export_root, name, info)?)))
        .collect::<Result<Vec<_>>>()?;

    let mut report = ImportReport::default();
    for (name, worktrees) in exported {
        if !projects.is_empty() && !projects.iter().any(|p| p == name) {
            continue;
        }
        let Ok(local) = ProjectEntry::load(projects_dir, name) else {
            report.miss(name, "not registered locally");
            continue;
        };
        if !local.presence().is_here() {
            report.miss(name, "not restored here");
            continue;
        }
        let root = local.root_path();
        let config = harness_config_in(Some(&root), global);
        let store = SessionStore {
            home,
            config: &config,
        };
        for wt in worktrees {
            let (label, to) = match wt.feature {
                None => (name.to_string(), paths::main_worktree(&root)),
                Some(feature) => {
                    let label = format!("{name}/{feature}");
                    if !FeatureState::exists(&paths::features_dir(&root), feature) {
                        report.miss(&label, "not a feature here");
                        continue;
                    }
                    let to = root.join(feature);
                    if !to.is_dir() {
                        report.miss(&label, "no worktree here");
                        continue;
                    }
                    (label, to)
                }
            };
            if !wt.sessions.exists() {
                report.miss(&label, "session data not found in tarball");
                continue;
            }
            match harness.import_sessions(&store, &wt.sessions, wt.from, &to)? {
                ImportOutcome::Skipped(why) => {
                    report.messages.push(format!("Skipping '{label}': {why}"))
                }
                ImportOutcome::Imported { detail, notes } => {
                    report
                        .messages
                        .extend(notes.iter().map(|note| format!("  {label}: {note}")));
                    report
                        .messages
                        .push(format!("Imported '{label}' ({detail})"));
                }
            }
        }
    }

    Ok(report)
}

/// One worktree's sessions in an export.
struct Worktree<'a> {
    /// `None` for main.
    feature: Option<&'a str>,
    /// Where the sessions were recorded.
    from: &'a Path,
    /// Their directory in the extracted export.
    sessions: PathBuf,
}

/// The worktrees project `name`'s manifest entry `info` carries, refusing
/// an entry with none and any string that would not stay where it is
/// joined. Main is absent when only features had sessions.
fn worktrees<'a>(
    export_root: &Path,
    name: &'a str,
    info: &'a serde_json::Value,
) -> Result<Vec<Worktree<'a>>> {
    let refused = |what: String| {
        PmError::ExportImport(format!("invalid export: project '{name}' has {what}"))
    };
    if !is_single_name(name) {
        return Err(refused("a name that is a path".to_string()));
    }
    let worktree = |feature: Option<&'a str>, entry: &'a serde_json::Value| {
        let of = feature
            .map(|f| format!(" for feature '{f}'"))
            .unwrap_or_default();
        let field = |key: &str| {
            entry[key].as_str().ok_or_else(|| {
                PmError::ExportImport(format!("missing '{key}' for project '{name}'{of}"))
            })
        };
        let (path, key) = (field("path")?, field("key")?);
        if !is_single_name(key) {
            return Err(refused(format!(
                "key '{key}'{of}, which is not a directory of the export"
            )));
        }
        let from = Path::new(path);
        if !from.is_absolute() {
            return Err(refused(format!("path '{path}'{of}, which is not absolute")));
        }
        Ok(Worktree {
            feature,
            from,
            sessions: export_root.join(PROJECTS_DIR).join(key),
        })
    };
    let mut out = Vec::new();
    if info.get("key").is_some() {
        out.push(worktree(None, info)?);
    }
    if let Some(features) = info.get("features") {
        let features = features
            .as_object()
            .ok_or_else(|| refused("'features' that is not an object".to_string()))?;
        for (feature, entry) in features {
            if !is_single_name(feature) {
                return Err(refused(format!("feature '{feature}', which is a path")));
            }
            out.push(worktree(Some(feature), entry)?);
        }
    }
    if out.is_empty() {
        return Err(refused("no sessions".to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::harness_export::tests::{
        add_feature, register, run_export, setup_claude_sessions, setup_project,
    };
    use tempfile::tempdir;

    fn run_import(
        harness: Harness,
        tarball: &Path,
        projects_dir: &Path,
        home: &Path,
    ) -> Result<Vec<String>> {
        import(
            Some(harness),
            tarball,
            &[],
            projects_dir,
            home,
            &HarnessConfig::default(),
        )
        .map(|report| report.messages)
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
            .join(crate::testing::claude_key(main))
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

        assert_eq!(msgs, ["Imported 'myapp' (1 session(s))"]);
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
            Some("Imported 'myapp' (1 session(s), path rewritten)")
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

        let report = import(
            Some(Harness::ClaudeCode),
            &source.tarball,
            &[],
            registry.path(),
            home.path(),
            &HarnessConfig::default(),
        )
        .unwrap();

        assert_eq!(
            report.messages,
            ["Skipping 'myapp': not registered locally"]
        );
        assert_eq!(report.missed, ["'myapp' (not registered locally)"]);
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
            [
                "Skipping 'myapp': all 1 session(s) already exist locally; kept the local \
                 sessions-index.json; the exported one differs"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(existing.join("session.jsonl")).unwrap(),
            "local history\n"
        );
    }

    #[test]
    fn feature_worktree_sessions_travel_with_their_project() {
        let source_home = tempdir().unwrap();
        let source_project = tempdir().unwrap();
        let source_registry = tempdir().unwrap();
        let out = tempdir().unwrap();
        let source_root = source_project.path().canonicalize().unwrap();
        let source_main = setup_project(&source_root, "myapp", source_registry.path());
        setup_claude_sessions(source_home.path(), &source_main);
        for feature in ["login", "gone", "unknown"] {
            let wt = add_feature(&source_root, feature);
            setup_claude_sessions(source_home.path(), &wt);
        }
        add_feature(&source_root, "idle");
        let (tarball, msgs) = run_export(
            Harness::ClaudeCode,
            Some(&source_root),
            source_registry.path(),
            &out.path().join("export.tar.gz"),
            source_home.path(),
        )
        .unwrap();
        for exported in ["myapp", "myapp/login", "myapp/gone", "myapp/unknown"] {
            let line = format!("Exported '{exported}' (");
            assert!(msgs.iter().any(|m| m.starts_with(&line)), "{msgs:?}");
        }

        // Restored elsewhere: `gone` is registered but has no worktree, and
        // `unknown` was never a feature here.
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let local_main = setup_project(&root, "myapp", registry.path());
        let local_login = add_feature(&root, "login");
        add_feature(&root, "gone");
        std::fs::remove_dir(root.join("gone")).unwrap();

        let mut msgs =
            run_import(Harness::ClaudeCode, &tarball, registry.path(), home.path()).unwrap();
        msgs.sort();
        assert_eq!(
            msgs,
            [
                "Imported 'myapp' (1 session(s), path rewritten)",
                "Imported 'myapp/login' (1 session(s), path rewritten)",
                "Skipping 'myapp/gone': no worktree here",
                "Skipping 'myapp/unknown': not a feature here",
            ]
        );
        for local in [&local_main, &local_login] {
            let content = std::fs::read_to_string(
                claude_sessions_of(home.path(), local).join("session.jsonl"),
            )
            .unwrap();
            assert!(content.contains(&*local.to_string_lossy()), "{content}");
        }
    }

    #[test]
    fn a_project_whose_only_sessions_are_a_features_is_exported() {
        let source_home = tempdir().unwrap();
        let source_project = tempdir().unwrap();
        let source_registry = tempdir().unwrap();
        let out = tempdir().unwrap();
        let source_root = source_project.path().canonicalize().unwrap();
        setup_project(&source_root, "myapp", source_registry.path());
        let wt = add_feature(&source_root, "login");
        setup_claude_sessions(source_home.path(), &wt);
        let (tarball, _) = run_export(
            Harness::ClaudeCode,
            Some(&source_root),
            source_registry.path(),
            &out.path().join("export.tar.gz"),
            source_home.path(),
        )
        .unwrap();

        let home = tempdir().unwrap();
        let registry = tempdir().unwrap();
        register(&source_root, "myapp", registry.path());
        let msgs = run_import(Harness::ClaudeCode, &tarball, registry.path(), home.path()).unwrap();
        assert_eq!(msgs, ["Imported 'myapp/login' (1 session(s))"]);
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
            Some("Imported 'myapp' (1 session(s), path rewritten)")
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

    /// A Claude Code export of `myapp` with this manifest entry, after
    /// `prepare` has had its way with the export's root directory.
    fn crafted_export(dir: &Path, entry: &str, prepare: impl Fn(&Path)) -> PathBuf {
        let root = dir.join("pm-claude-export");
        let sessions = root.join("projects/-old-myapp-main");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("session.jsonl"), "{}\n").unwrap();
        std::fs::write(
            root.join("manifest.json"),
            format!(r#"{{"myapp": {entry}}}"#),
        )
        .unwrap();
        prepare(&root);
        let tarball = dir.join("crafted.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf", &tarball.to_string_lossy(), "-C"])
            .arg(dir)
            .arg("pm-claude-export")
            .status()
            .unwrap();
        assert!(status.success());
        tarball
    }

    /// Import a crafted export on a machine that has `myapp` registered and
    /// a file outside the export to reach for. Returns the error and whether
    /// the session store was left untouched.
    fn import_crafted(entry: &str, prepare: impl Fn(&Path, &Path)) -> (String, bool) {
        let staging = tempdir().unwrap();
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let registry = tempdir().unwrap();
        setup_project(project.path(), "myapp", registry.path());
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.jsonl"), "secret\n").unwrap();

        let entry = entry.replace("OUTSIDE", &outside.to_string_lossy());
        let tarball = crafted_export(staging.path(), &entry, |root| prepare(root, &outside));
        let err = run_import(Harness::ClaudeCode, &tarball, registry.path(), home.path())
            .unwrap_err()
            .to_string();
        (err, !home.path().join(".claude").exists())
    }

    #[test]
    fn import_refuses_a_key_that_leaves_the_export() {
        for key in ["../../../outside", "OUTSIDE", "a/../b", ".", ""] {
            let entry = format!(r#"{{"path": "/old/myapp/main", "key": "{key}"}}"#);
            let (err, untouched) = import_crafted(&entry, |_, _| {});
            assert!(
                err.contains("project 'myapp' has key") && untouched,
                "{key}: {err}"
            );
        }
    }

    #[test]
    fn import_refuses_a_feature_name_or_key_that_leaves_the_export() {
        let main = r#""path": "/old/myapp/main", "key": "-old-myapp-main""#;
        for (feature, key, refusal) in [
            (
                "../login",
                "-old-myapp-login",
                "feature '../login', which is a path",
            ),
            (
                "login",
                "../outside",
                "key '../outside' for feature 'login'",
            ),
        ] {
            let entry = format!(
                r#"{{{main}, "features": {{"{feature}": {{"path": "/old/myapp/{feature}", "key": "{key}"}}}}}}"#
            );
            let (err, untouched) = import_crafted(&entry, |_, _| {});
            assert!(err.contains(refusal) && untouched, "{err}");
        }
    }

    #[test]
    fn import_refuses_a_project_entry_without_sessions() {
        for entry in [
            r#"{"path": "/old/myapp/main"}"#,
            r#"{"path": "/old/myapp/main", "features": {}}"#,
        ] {
            let (err, untouched) = import_crafted(entry, |_, _| {});
            assert!(
                err.ends_with("project 'myapp' has no sessions") && untouched,
                "{err}"
            );
        }
    }

    #[test]
    fn import_refuses_a_recorded_path_that_is_not_absolute() {
        let (err, untouched) =
            import_crafted(r#"{"path": "..", "key": "-old-myapp-main"}"#, |_, _| {});
        assert!(
            err.contains("project 'myapp' has path '..'") && untouched,
            "{err}"
        );
    }

    #[test]
    fn import_refuses_an_export_holding_links() {
        let entry = r#"{"path": "/old/myapp/main", "key": "-old-myapp-main"}"#;
        let sessions = "projects/-old-myapp-main";

        let (err, untouched) = import_crafted(entry, |root, outside| {
            std::os::unix::fs::symlink(
                outside.join("secret.jsonl"),
                root.join(sessions).join("stolen.jsonl"),
            )
            .unwrap();
        });
        assert!(
            err.ends_with(
                "'pm-claude-export/projects/-old-myapp-main/stolen.jsonl' is a link or special file"
            ) && untouched,
            "{err}"
        );

        // The sessions directory itself, pointing out of the export.
        let (err, untouched) = import_crafted(entry, |root, outside| {
            std::fs::remove_dir_all(root.join(sessions)).unwrap();
            std::os::unix::fs::symlink(outside, root.join(sessions)).unwrap();
        });
        assert!(
            err.contains("is a link or special file") && untouched,
            "{err}"
        );

        let (err, untouched) = import_crafted(entry, |root, _| {
            std::fs::hard_link(
                root.join(sessions).join("session.jsonl"),
                root.join(sessions).join("twin.jsonl"),
            )
            .unwrap();
        });
        assert!(
            err.contains("is a link or special file") && untouched,
            "{err}"
        );
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
                projects: &[],
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
        // No harness given: the tarball says it holds opencode sessions.
        let msgs = import(
            None,
            &tarball,
            &[],
            registry.path(),
            project.path(),
            &opencode(fake_opencode(local_opencode.path(), "", 0)),
        )
        .unwrap();

        assert_eq!(msgs.messages, ["Imported 'myapp' (1 session(s))"]);
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
