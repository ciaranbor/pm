//! `pm harness export`: a tarball of the sessions recorded at each
//! project's main and feature worktrees,
//! `pm-<harness>-export/{manifest.json, projects/<key>/…}`. The manifest maps
//! a project name to the harness, the path main's sessions were recorded at
//! with the directory holding them in the tarball (`key`, absent when main
//! has none), and `features`: per feature with sessions, its own `path` and
//! `key`. What a key's directory contains is the harness's own
//! ([`Harness::export_sessions`]).

use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::harness::{ExportJob, Harness};
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectConfig, ProjectEntry, harness_config_in};

pub(super) const MANIFEST: &str = "manifest.json";
pub(super) const PROJECTS_DIR: &str = "projects";

pub struct ExportParams<'a> {
    pub harness: Harness,
    /// The project to export; unused with `all`.
    pub project_root: Option<&'a Path>,
    pub projects_dir: &'a Path,
    pub all: bool,
    pub output: Option<&'a Path>,
    pub home: &'a Path,
    /// The global tier's `[harness.*]` settings.
    pub global: &'a HarnessConfig,
}

/// The tarball's root directory.
pub(super) fn export_root(harness: Harness) -> String {
    format!("pm-{}-export", harness.export_tag())
}

/// A project to export: its name, root, and the worktrees on disk whose
/// sessions it carries.
struct Project {
    name: String,
    root: PathBuf,
    worktrees: Vec<Worktree>,
}

/// One worktree of a project: `None` for main, else the feature's name.
struct Worktree {
    feature: Option<String>,
    /// Resolved: the form every harness records a session's directory in,
    /// and so the one an import has to find in the transcripts.
    recorded: PathBuf,
}

fn resolved(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn project(name: String, root: PathBuf) -> Result<Project> {
    let mut worktrees = vec![Worktree {
        feature: None,
        recorded: resolved(paths::main_worktree(&root)),
    }];
    for (scope, path) in super::skills::scoped_worktrees_on_disk(&root)? {
        if scope != "main" {
            worktrees.push(Worktree {
                feature: Some(scope),
                recorded: resolved(path),
            });
        }
    }
    Ok(Project {
        name,
        root,
        worktrees,
    })
}

fn resolve_projects(
    project_root: Option<&Path>,
    projects_dir: &Path,
    all: bool,
) -> Result<Vec<Project>> {
    if all {
        // Skipping an entry would drop a project from the migration bundle
        // unnoticed until the other machine.
        let registry = ProjectEntry::scan(projects_dir)?;
        if !registry.malformed.is_empty() {
            let entries: Vec<String> = registry
                .malformed
                .iter()
                .map(|bad| format!("{} ({})", bad.path.display(), bad.error))
                .collect();
            return Err(PmError::ExportImport(format!(
                "unreadable registry entries, fix or remove them first: {}",
                entries.join("; ")
            )));
        }
        let entries = registry.projects;
        if entries.is_empty() {
            return Err(PmError::ExportImport("no projects registered".to_string()));
        }
        entries
            .into_iter()
            .map(|(name, entry)| project(name, entry.root_path()))
            .collect()
    } else {
        let root = project_root.ok_or(PmError::NotInProject)?;
        let config = ProjectConfig::load(&paths::pm_dir(root))?;
        Ok(vec![project(config.project.name, root.to_path_buf())?])
    }
}

/// The directory a project's sessions occupy in the tarball: any name
/// unique to the path, recorded in the manifest for the import to read.
fn staging_key(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches('/')
        .replace('/', "-")
}

/// Export one or all projects' sessions into a tarball.
///
/// Returns the path to the created tarball and a list of status messages.
pub fn export(params: &ExportParams<'_>) -> Result<(PathBuf, Vec<String>)> {
    let harness = params.harness;
    let projects = resolve_projects(params.project_root, params.projects_dir, params.all)?;

    let staging = tempfile::tempdir()?;
    let root_name = export_root(harness);
    let staging_root = staging.path().join(&root_name);
    let staging_projects = staging_root.join(PROJECTS_DIR);
    std::fs::create_dir_all(&staging_projects)?;

    let mut messages = Vec::new();
    let mut manifest = serde_json::Map::new();
    let mut exported = Vec::new();
    let configs: Vec<HarnessConfig> = projects
        .iter()
        .map(|project| harness_config_in(Some(&project.root), params.global))
        .collect();
    let jobs: Vec<ExportJob<'_>> = projects
        .iter()
        .zip(&configs)
        .flat_map(|(project, config)| {
            project.worktrees.iter().map(|wt| ExportJob {
                config,
                dir: &wt.recorded,
                staging: staging_projects.join(staging_key(&wt.recorded)),
            })
        })
        .collect();
    let mut details = harness.export_sessions(params.home, &jobs)?.into_iter();
    for project in &projects {
        let name = &project.name;
        let mut entry = serde_json::json!({
            "path": project.worktrees[0].recorded.to_string_lossy(),
            "harness": harness.as_str(),
        });
        let mut features = serde_json::Map::new();
        for wt in &project.worktrees {
            let Some(detail) = details.next().flatten() else {
                continue;
            };
            let key = staging_key(&wt.recorded);
            match &wt.feature {
                None => {
                    messages.push(format!("Exported '{name}' ({detail})"));
                    entry["key"] = key.into();
                }
                Some(feature) => {
                    messages.push(format!("Exported '{name}/{feature}' ({detail})"));
                    features.insert(
                        feature.clone(),
                        serde_json::json!({
                            "path": wt.recorded.to_string_lossy(),
                            "key": key,
                        }),
                    );
                }
            }
        }
        if entry.get("key").is_none() && features.is_empty() {
            messages.push(format!("Skipping '{name}': no {harness} sessions found"));
            continue;
        }
        if !features.is_empty() {
            entry["features"] = features.into();
        }
        manifest.insert(name.clone(), entry);
        exported.push(name.as_str());
    }

    if exported.is_empty() {
        return Err(PmError::ExportImport(format!(
            "no {harness} sessions found for any project"
        )));
    }

    std::fs::write(
        staging_root.join(MANIFEST),
        serde_json::to_string_pretty(&serde_json::Value::Object(manifest))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    )?;

    let output_path = match params.output {
        Some(p) => p.to_path_buf(),
        None => {
            let tag = harness.export_tag();
            let name = match exported[..] {
                [only] => format!("pm-{tag}-{only}.tar.gz"),
                _ => format!("pm-{tag}-export.tar.gz"),
            };
            std::env::current_dir()?.join(name)
        }
    };

    let status = std::process::Command::new("tar")
        .args([
            "-czf",
            &output_path.to_string_lossy(),
            "-C",
            &staging.path().to_string_lossy(),
            &root_name,
        ])
        .status()?;
    if !status.success() {
        return Err(PmError::ExportImport("tar command failed".to_string()));
    }

    messages.push(format!("Created {}", output_path.display()));
    Ok((output_path, messages))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Claude Code sessions recorded at `project_path`, in the store under
    /// `home`.
    pub fn setup_claude_sessions(home: &Path, project_path: &Path) -> PathBuf {
        let dir = home
            .join(".claude/projects")
            .join(crate::testing::claude_key(project_path));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", project_path.display()),
        )
        .unwrap();
        std::fs::write(
            dir.join("sessions-index.json"),
            format!(
                "[{{\"sessionId\":\"abc\",\"fullPath\":\"{}\"}}]",
                project_path.display()
            ),
        )
        .unwrap();
        dir
    }

    /// A registered project at `root`; returns its main worktree.
    pub fn setup_project(root: &Path, name: &str, projects_dir: &Path) -> PathBuf {
        let root = &root.canonicalize().unwrap();
        let pm_dir = root.join(".pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        let config = ProjectConfig {
            project: crate::state::project::ProjectInfo {
                name: name.to_string(),
                max_features: None,
            },
            agents: Default::default(),
            harness: Default::default(),
        };
        config.save(&pm_dir).unwrap();
        let main_path = paths::main_worktree(root);
        std::fs::create_dir_all(&main_path).unwrap();
        register(root, name, projects_dir);
        main_path
    }

    /// A registered feature of the project at `root` with its worktree;
    /// returns the worktree.
    pub fn add_feature(root: &Path, name: &str) -> PathBuf {
        let now = chrono::Utc::now();
        crate::state::feature::FeatureState {
            status: crate::state::feature::FeatureStatus::Wip,
            progress: Default::default(),
            blocked_reason: None,
            blocked_by: None,
            branch: name.to_string(),
            worktree: name.to_string(),
            base: String::new(),
            pr: String::new(),
            context: String::new(),
            workflow: None,
            created: now,
            last_active: now,
        }
        .save(&paths::features_dir(root), name)
        .unwrap();
        let worktree = root.join(name);
        std::fs::create_dir_all(&worktree).unwrap();
        worktree
    }

    pub fn register(root: &Path, name: &str, projects_dir: &Path) {
        let entry = ProjectEntry {
            root: root.to_string_lossy().to_string(),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        entry.save(projects_dir, name).unwrap();
    }

    /// Export `project_root` (or every project) from the store under `home`.
    pub fn run_export(
        harness: Harness,
        project_root: Option<&Path>,
        projects_dir: &Path,
        output: &Path,
        home: &Path,
    ) -> Result<(PathBuf, Vec<String>)> {
        export(&ExportParams {
            harness,
            project_root,
            projects_dir,
            all: project_root.is_none(),
            output: Some(output),
            home,
            global: &HarnessConfig::default(),
        })
    }

    /// The manifest of the tarball at `path`.
    fn manifest_of(path: &Path, harness: Harness) -> serde_json::Value {
        let out = tempdir().unwrap();
        let status = std::process::Command::new("tar")
            .args(["-xzf", &path.to_string_lossy(), "-C"])
            .arg(out.path())
            .status()
            .unwrap();
        assert!(status.success());
        let manifest = out.path().join(export_root(harness)).join(MANIFEST);
        serde_json::from_str(&std::fs::read_to_string(manifest).unwrap()).unwrap()
    }

    #[test]
    fn export_single_project_records_where_its_sessions_came_from() {
        let home = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let main_path = setup_project(project_dir.path(), "myapp", projects_dir.path());
        setup_claude_sessions(home.path(), &main_path);

        let output_path = output_dir.path().join("export.tar.gz");
        let (path, msgs) = run_export(
            Harness::ClaudeCode,
            Some(project_dir.path()),
            projects_dir.path(),
            &output_path,
            home.path(),
        )
        .unwrap();

        assert_eq!(path, output_path);
        assert!(msgs.iter().any(|m| m.contains("Exported 'myapp'")));
        assert_eq!(
            manifest_of(&output_path, Harness::ClaudeCode),
            serde_json::json!({"myapp": {
                "path": main_path.to_string_lossy(),
                "key": staging_key(&main_path),
                "harness": "claude-code",
            }})
        );
    }

    #[test]
    fn export_all_skips_projects_without_sessions() {
        let home = tempdir().unwrap();
        let project_a = tempdir().unwrap();
        let project_b = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();

        let main_a = setup_project(project_a.path(), "alpha", projects_dir.path());
        setup_project(project_b.path(), "beta", projects_dir.path());
        setup_claude_sessions(home.path(), &main_a);

        let output_path = output_dir.path().join("partial.tar.gz");
        let (_, msgs) = run_export(
            Harness::ClaudeCode,
            None,
            projects_dir.path(),
            &output_path,
            home.path(),
        )
        .unwrap();

        assert!(
            msgs.iter()
                .any(|m| m == "Skipping 'beta': no claude-code sessions found"),
            "{msgs:?}"
        );
        let manifest = manifest_of(&output_path, Harness::ClaudeCode);
        let names: Vec<_> = manifest.as_object().unwrap().keys().collect();
        assert_eq!(names, ["alpha"]);
    }

    #[test]
    fn export_all_refuses_an_unreadable_registry_entry() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let main = setup_project(project.path(), "alpha", projects_dir.path());
        setup_claude_sessions(home.path(), &main);
        let bad = projects_dir.path().join("beta.toml");
        std::fs::write(&bad, "root = ").unwrap();

        let output_path = output_dir.path().join("all.tar.gz");
        let err = run_export(
            Harness::ClaudeCode,
            None,
            projects_dir.path(),
            &output_path,
            home.path(),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains(&bad.display().to_string()), "{err}");
        assert!(!output_path.exists());
    }

    #[test]
    fn export_errors_when_no_sessions_found() {
        let home = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let projects_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let main_path = setup_project(project_dir.path(), "empty", projects_dir.path());
        // Sessions of another harness do not count.
        setup_claude_sessions(home.path(), &main_path);

        let output_path = output_dir.path().join("none.tar.gz");
        let err = run_export(
            Harness::Codex,
            Some(project_dir.path()),
            projects_dir.path(),
            &output_path,
            home.path(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("no codex sessions found for any project"),
            "{err}"
        );
        assert!(!output_path.exists());
    }
}
