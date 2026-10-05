//! `pm migrate check`: what moving projects to another machine with the
//! docs/migration.md steps would lose or fail on, for the selected
//! projects and the global registry. It only reads: it never
//! pushes, commits, fetches or changes pm state, and it asks each remote
//! what it holds (`git ls-remote`) rather than trusting the last fetch.
//!
//! A finding is a blocker (work that would be lost, or a step that would
//! fail), a manual step (something machine-local nothing pm syncs carries),
//! or a note. Each subject — a worktree, `.pm/`, a registry entry, the
//! running agents — gets one blocker naming all its problems, so the report
//! reads as one line per thing. A blocker names the [`Step`]s of the plan
//! that clear it, and carries its own command only where no plan step does
//! (the git commands of one worktree). Project paths are written relative
//! to the project root, the directory every command is run from.

mod agents;
mod line;
mod machine;
mod plan;
mod project;
mod registry;
mod render;
mod repo;
mod worktree;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::commands::running_agents::Windows;
use crate::error::Result;
use crate::harness::{Harness, Probe};
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectEntry};
use crate::{path_utils, tmux};

pub use plan::{PlanStep, Step};
pub use render::Style;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Blocker,
    Manual,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// What it is about, as the report names it (`login/`, `.pm/`,
    /// `agents`); empty for the section's own repo.
    pub subject: String,
    /// The problems, in a few words.
    pub what: String,
    /// Specifics and explanations, shown with `--verbose`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    /// A command no plan step covers, run from the section's root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// The plan steps that clear a blocker.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// A feature with active agents and uncommitted work: better finished
    /// and merged before the move than committed half done.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub in_flight: bool,
}

impl Finding {
    fn blocker(subject: &str, what: String, fix: Option<String>, steps: &[Step]) -> Self {
        Self {
            severity: Severity::Blocker,
            subject: subject.to_string(),
            what,
            detail: Vec::new(),
            fix,
            steps: steps.to_vec(),
            in_flight: false,
        }
    }

    fn manual(subject: &str, what: String, detail: Option<String>) -> Self {
        Self {
            severity: Severity::Manual,
            subject: subject.to_string(),
            what,
            detail: detail.into_iter().collect(),
            fix: None,
            steps: Vec::new(),
            in_flight: false,
        }
    }

    fn note(subject: &str, what: String) -> Self {
        Self {
            severity: Severity::Note,
            subject: subject.to_string(),
            what,
            detail: Vec::new(),
            fix: None,
            steps: Vec::new(),
            in_flight: false,
        }
    }
}

/// The subject of a machine manual step done on this host before leaving
/// it; every other manual step is done on the new host.
const THIS_HOST: &str = "this host";

/// The harnesses agents run on, each with the `[harness.*]` settings in
/// effect where it was found.
type Harnesses = Vec<(Harness, HarnessConfig)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionKind {
    Registry,
    Project,
    Machine,
}

#[derive(Debug, Serialize)]
pub struct Section {
    pub kind: SectionKind,
    pub name: String,
    /// Where it lives, `~/…` when under home; empty for the machine.
    pub path: String,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    pub fn blockers(&self) -> usize {
        self.findings()
            .filter(|f| f.severity == Severity::Blocker)
            .count()
    }

    fn findings(&self) -> impl Iterator<Item = &Finding> {
        self.sections.iter().flat_map(|s| &s.findings)
    }

    /// The report as text: `verbose` adds file names and explanations.
    pub fn lines(&self, style: Style, verbose: bool) -> Vec<String> {
        render::lines(self, style, verbose)
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "ready": self.blockers() == 0,
            "blockers": self.blockers(),
            "plan": plan::plan(self),
            "sections": self.sections,
        })
    }
}

pub struct CheckParams<'a> {
    pub projects_dir: &'a Path,
    /// The pm config dir: the global registry repo.
    pub config_dir: &'a Path,
    pub home: &'a Path,
    /// Registered names to check; empty checks every registered project.
    pub projects: &'a [String],
    pub tmux_server: Option<&'a str>,
    pub probe: Probe,
    /// The global tier's `[harness.*]` settings.
    pub global_harness: &'a HarnessConfig,
}

/// A path as a shell argument: `~/…` when that is how the registry writes
/// it and needs no quoting, else the quoted absolute path.
fn shell_path(path: &Path) -> String {
    let portable = path_utils::to_portable(path);
    if plain(&portable) {
        portable
    } else {
        tmux::shell_quote(&path.to_string_lossy())
    }
}

fn plain(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~+@".contains(c))
}

/// A path as a shell argument for a command run from `root`: relative when
/// under it, else as [`shell_path`] writes it.
fn rel_path(root: &Path, path: &Path) -> String {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let rel = path
        .strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            canonical(path)
                .strip_prefix(canonical(root))
                .ok()
                .map(Path::to_path_buf)
        });
    match rel {
        Some(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Some(rel) if plain(&rel.to_string_lossy()) => rel.to_string_lossy().into_owned(),
        Some(rel) => tmux::shell_quote(&rel.to_string_lossy()),
        None => shell_path(path),
    }
}

pub fn check(params: &CheckParams<'_>) -> Result<Report> {
    let registry = ProjectEntry::scan(params.projects_dir)?;
    let mut global = registry::findings(params.config_dir)?;
    for bad in &registry.malformed {
        let mut finding = Finding::blocker(
            &shell_path(&bad.path),
            "unreadable registry entry: fix or remove it".to_string(),
            None,
            &[Step::Repair],
        );
        finding.detail.push(bad.error.to_string());
        finding
            .detail
            .push("`pm harness export --all` refuses to run while it is there".to_string());
        global.push(finding);
    }

    let selected: Vec<(String, Option<ProjectEntry>)> = if params.projects.is_empty() {
        registry
            .projects
            .iter()
            .map(|(name, entry)| (name.clone(), Some(entry.clone())))
            .collect()
    } else {
        params
            .projects
            .iter()
            .map(|name| {
                let entry = registry
                    .projects
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, e)| e.clone());
                (name.clone(), entry)
            })
            .collect()
    };
    let others: Vec<&str> = registry
        .projects
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !selected.iter().any(|(s, _)| s == name))
        .collect();
    if !others.is_empty() {
        global.push(Finding::note(
            "",
            format!(
                "not checked, but in the registry the new host pulls: {}; `pm restore` with the \
                 same `--project` flags restores only these",
                others.join(", ")
            ),
        ));
    }

    let windows = Windows::read(params.tmux_server).ok();
    let mut harnesses: Harnesses = Vec::new();
    let mut sections = vec![Section {
        kind: SectionKind::Registry,
        name: "global registry".to_string(),
        path: shell_path(params.config_dir),
        findings: global,
    }];
    for (name, entry) in &selected {
        let (findings, used) = match entry {
            None => (
                vec![Finding::blocker(
                    "",
                    "not a registered project; `pm list` names them".to_string(),
                    None,
                    &[Step::Repair],
                )],
                Vec::new(),
            ),
            Some(entry) => project::findings(name, entry, windows.as_ref(), params.global_harness)?,
        };
        for (harness, config) in used {
            if !harnesses.contains(&(harness, config.clone())) {
                harnesses.push((harness, config));
            }
        }
        sections.push(Section {
            kind: SectionKind::Project,
            name: name.clone(),
            path: entry.as_ref().map(|e| e.root.clone()).unwrap_or_default(),
            findings,
        });
    }

    let machine = machine::findings(&machine::Machine {
        home: params.home,
        config_dir: params.config_dir,
        harnesses: &harnesses,
        projects: params.projects,
        probe: params.probe,
    })?;
    sections.push(Section {
        kind: SectionKind::Machine,
        name: "this machine".to_string(),
        path: String::new(),
        findings: machine,
    });
    Ok(Report { sections })
}

/// The pm config dir, home, and registry for a check of this machine.
pub fn resolve() -> Result<(PathBuf, PathBuf, PathBuf)> {
    Ok((
        paths::global_config_dir()?,
        paths::home_dir()?,
        paths::global_projects_dir()?,
    ))
}
