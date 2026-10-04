//! `pm migrate check`: what moving projects to another machine with the
//! README's "Moving to another machine" steps would lose or fail on, for the
//! selected projects and the global registry. It only reads: it never
//! pushes, commits, fetches or changes pm state, and it asks each remote
//! what it holds (`git ls-remote`) rather than trusting the last fetch.
//!
//! A finding is a blocker (work that would be lost, or a step that would
//! fail), a manual step (something machine-local nothing pm syncs carries),
//! or a note. Every blocker names the command that clears it.

mod machine;
mod project;
mod repo;
mod worktree;

use std::path::{Path, PathBuf};

use crate::commands::running_agents::Windows;
use crate::error::Result;
use crate::harness::{Harness, Probe};
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectEntry};
use crate::{git, path_utils, tmux};

use repo::Remote;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Blocker,
    Manual,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub what: String,
    /// The command that clears a blocker.
    pub fix: Option<String>,
}

impl Finding {
    fn blocker(what: String, fix: String) -> Self {
        Self {
            severity: Severity::Blocker,
            what,
            fix: Some(fix),
        }
    }

    fn manual(what: String) -> Self {
        Self {
            severity: Severity::Manual,
            what,
            fix: None,
        }
    }

    fn note(what: String) -> Self {
        Self {
            severity: Severity::Note,
            what,
            fix: None,
        }
    }
}

/// The harnesses agents run on, each with the `[harness.*]` settings in
/// effect where it was found.
type Harnesses = Vec<(Harness, HarnessConfig)>;

pub struct Section {
    pub title: String,
    pub findings: Vec<Finding>,
}

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

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for section in &self.sections {
            out.push(section.title.clone());
            if section.findings.is_empty() {
                out.push("  ok".to_string());
            }
            for f in &section.findings {
                let tag = match f.severity {
                    Severity::Blocker => "BLOCKER",
                    Severity::Manual => "manual ",
                    Severity::Note => "note   ",
                };
                out.push(format!("  {tag} {}", f.what));
                if let Some(fix) = &f.fix {
                    out.push(format!("          fix: {fix}"));
                }
            }
        }
        let manual = self
            .findings()
            .filter(|f| f.severity == Severity::Manual)
            .count();
        out.push(String::new());
        out.push(match self.blockers() {
            0 => format!("Ready to migrate: no blockers; {manual} manual steps above."),
            n => format!(
                "Not ready: {n} blocker{}. Clear them in this order — stop agents, commit and \
                 push repos, `pm state push` in each project, `pm state backfill`, then `pm \
                 state push --global` — and run this check again.",
                if n == 1 { "" } else { "s" }
            ),
        });
        out
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
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-~+@".contains(c))
    };
    if plain(&portable) {
        portable
    } else {
        tmux::shell_quote(&path.to_string_lossy())
    }
}

pub fn check(params: &CheckParams<'_>) -> Result<Report> {
    let registry = ProjectEntry::scan(params.projects_dir)?;
    let mut global = registry_findings(params.config_dir)?;
    for bad in &registry.malformed {
        global.push(Finding::blocker(
            format!(
                "unreadable registry entry {} ({}): `pm harness export --all` refuses to run",
                shell_path(&bad.path),
                bad.error
            ),
            format!("fix or remove {}", shell_path(&bad.path)),
        ));
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
        global.push(Finding::note(format!(
            "not checked, but in the registry the new host pulls: {}; pass the same `--project` \
             flags to `pm restore` to restore only these",
            others.join(", ")
        )));
    }

    let windows = Windows::read(params.tmux_server).ok();
    let mut harnesses: Harnesses = Vec::new();
    let mut sections = vec![Section {
        title: format!("global registry ({})", shell_path(params.config_dir)),
        findings: global,
    }];
    for (name, entry) in &selected {
        let (findings, used) = match entry {
            None => (
                vec![Finding::blocker(
                    format!("'{name}' is not a registered project"),
                    "`pm list` names the registered projects".to_string(),
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
        let title = match entry {
            Some(entry) => format!("project {name} ({})", entry.root),
            None => format!("project {name}"),
        };
        sections.push(Section { title, findings });
    }

    let mut machine = Vec::new();
    let mut exported: Vec<Harness> = Vec::new();
    for (harness, _) in &harnesses {
        if !exported.contains(harness) {
            exported.push(*harness);
        }
    }
    for harness in &exported {
        machine.push(Finding::manual(format!(
            "{harness} conversations travel only in an export: `pm close --all`, then `pm \
             harness export --all --harness {harness} -o pm-{harness}.tar.gz`, then `pm \
             restore --import pm-{harness}.tar.gz` on the new host"
        )));
    }
    machine.extend(machine::findings(
        params.home,
        params.config_dir,
        &harnesses,
        params.probe,
    ));
    sections.push(Section {
        title: "this machine (manual steps on the new host)".to_string(),
        findings: machine,
    });
    Ok(Report { sections })
}

/// The global registry repo: present, with a remote, clean, and pushed.
fn registry_findings(config_dir: &Path) -> Result<Vec<Finding>> {
    if !git::is_git_repo(config_dir) {
        return Ok(vec![Finding::blocker(
            "the global registry is not a git repo, so the new host can't pull it".to_string(),
            "pm state init --global --remote <new empty repo url>".to_string(),
        )]);
    }
    let push = "pm state push --global";
    let mut out = Vec::new();
    let remote = Remote::of(config_dir)?;
    if remote.is_none() {
        out.push(Finding::blocker(
            "the global registry has no remote".to_string(),
            format!("pm state remote --global <new empty repo url>, then {push}"),
        ));
    }
    out.extend(repo::state_dirty_finding(
        config_dir,
        "global registry",
        push,
    )?);
    if let Some(url) = git::remote_url(config_dir, "origin")? {
        out.push(Finding::manual(format!(
            "on the new host, pull the registry from exactly this remote: `pm state init \
             --global --remote {url}`"
        )));
    }
    if let Some(remote) = &remote {
        let branch = git::current_branch(config_dir)?;
        out.extend(repo::branch_finding(
            config_dir,
            remote,
            &branch,
            "global registry",
            push,
            Some("pm state pull --global"),
        )?);
    }
    Ok(out)
}

/// The pm config dir, home, and registry for a check of this machine.
pub fn resolve() -> Result<(PathBuf, PathBuf, PathBuf)> {
    Ok((
        paths::global_config_dir()?,
        paths::home_dir()?,
        paths::global_projects_dir()?,
    ))
}
