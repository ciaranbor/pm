//! The ordered actions that clear every blocker: each blocker names the
//! steps it needs, and the plan runs each step once, for every project
//! that needs it.

use serde::Serialize;

use super::{Report, SectionKind, Severity};

/// The placeholder for the registry's remote, in the plan and on the new
/// host alike.
pub(super) const REGISTRY_URL: &str = "<registry repo url>";

/// A plan step, in the order the plan runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    /// Agents keep writing `.pm/` and their conversations: stop them first.
    StopAgents,
    /// A problem its own line says how to resolve, before the rest.
    Repair,
    /// Commit, rebase or push a worktree's branch: its own git command.
    Branches,
    StateInit,
    StateRemote,
    StatePush,
    RegistryInit,
    RegistryRemote,
    Backfill,
    GlobalPush,
}

impl Step {
    /// The command the step runs, where one command does it all.
    pub fn command(self) -> Option<&'static str> {
        Some(match self {
            Step::StopAgents => "pm close --all",
            Step::Repair | Step::Branches => return None,
            Step::StateInit => "pm state init --remote <new empty repo url>",
            Step::StateRemote => "pm state remote <new empty repo url>",
            Step::StatePush => "pm state push",
            Step::RegistryInit => "pm state init --global --remote <registry repo url>",
            Step::RegistryRemote => "pm state remote --global <registry repo url>",
            Step::Backfill => "pm state backfill",
            Step::GlobalPush => "pm state push --global",
        })
    }

    /// Whether the command runs in each project's root.
    fn per_project(self) -> bool {
        matches!(self, Step::StateInit | Step::StateRemote | Step::StatePush)
    }
}

#[derive(Debug, Serialize)]
pub struct PlanStep {
    pub step: Step,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<&'static str>,
    /// How many blockers need it.
    pub blockers: usize,
    /// The projects to run it in, for a command run in a project's root.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
}

pub(super) fn plan(report: &Report) -> Vec<PlanStep> {
    let mut out: Vec<PlanStep> = Vec::new();
    for section in &report.sections {
        for finding in &section.findings {
            if finding.severity != Severity::Blocker {
                continue;
            }
            for &step in &finding.steps {
                let entry = match out.iter_mut().position(|p| p.step == step) {
                    Some(i) => &mut out[i],
                    None => {
                        out.push(PlanStep {
                            step,
                            command: step.command(),
                            blockers: 0,
                            projects: Vec::new(),
                        });
                        out.last_mut().expect("just pushed")
                    }
                };
                entry.blockers += 1;
                if step.per_project()
                    && section.kind == SectionKind::Project
                    && !entry.projects.contains(&section.name)
                {
                    entry.projects.push(section.name.clone());
                }
            }
        }
    }
    out.sort_by_key(|p| p.step);
    out
}
