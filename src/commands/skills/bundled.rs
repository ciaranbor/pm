//! The bundle itself: every asset pm ships, embedded at build time.

use std::fs;
use std::path::Path;

use crate::error::{PmError, Result};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundledKind {
    Skill,
    Agent,
    Workflow,
    /// Single shared "operating baseline" file appended to every spawned
    /// agent's system prompt.
    Baseline,
}

impl BundledKind {
    pub(super) const ALL: [BundledKind; 4] =
        [Self::Skill, Self::Agent, Self::Baseline, Self::Workflow];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Skill => "Skill",
            Self::Agent => "Agent",
            Self::Workflow => "Workflow",
            Self::Baseline => "Baseline",
        }
    }

    fn not_found_error(self, name: &str) -> PmError {
        match self {
            Self::Skill => PmError::SkillNotFound(name.to_string()),
            Self::Agent => PmError::AgentNotFound(name.to_string()),
            Self::Workflow => PmError::WorkflowNotFound(name.to_string()),
            Self::Baseline => PmError::BaselineNotFound(name.to_string()),
        }
    }

    /// Subdirectory of the canonical store (and of every harness projection)
    /// this kind lives in; `None` for kinds that aren't projected.
    pub(super) fn store_subdir(self) -> Option<&'static str> {
        match self {
            Self::Skill => Some("skills"),
            Self::Agent => Some("agents"),
            Self::Workflow | Self::Baseline => None,
        }
    }
}

pub(super) struct BundledItem {
    pub(super) kind: BundledKind,
    pub(super) name: &'static str,
    /// One or more files that make up this item, relative to the install
    /// directory. Skills and agents have exactly one file each; workflows
    /// have two (`config.toml` + `workflow.md`).
    pub(super) files: &'static [(&'static str, &'static str)],
}
pub(super) const BUNDLED_ITEMS: &[BundledItem] = &[
    // Skills
    BundledItem {
        kind: BundledKind::Skill,
        name: "pm",
        files: &[(
            "pm/SKILL.md",
            include_str!("../../../assets/skills/pm/SKILL.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Skill,
        name: "messaging",
        files: &[(
            "messaging/SKILL.md",
            include_str!("../../../assets/skills/messaging/SKILL.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Skill,
        name: "pm-workflow",
        files: &[(
            "pm-workflow/SKILL.md",
            include_str!("../../../assets/skills/pm-workflow/SKILL.md"),
        )],
    },
    // Agents
    BundledItem {
        kind: BundledKind::Agent,
        name: "reviewer",
        files: &[(
            "reviewer.md",
            include_str!("../../../assets/agents/reviewer.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "implementer",
        files: &[(
            "implementer.md",
            include_str!("../../../assets/agents/implementer.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "researcher",
        files: &[(
            "researcher.md",
            include_str!("../../../assets/agents/researcher.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "qa",
        files: &[("qa.md", include_str!("../../../assets/agents/qa.md"))],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "main",
        files: &[("main.md", include_str!("../../../assets/agents/main.md"))],
    },
    // Baseline (shared operating prompt appended to every spawned agent)
    BundledItem {
        kind: BundledKind::Baseline,
        name: "pm-baseline",
        files: &[(
            "pm-baseline.md",
            include_str!("../../../assets/baseline/pm-baseline.md"),
        )],
    },
    // Workflows
    BundledItem {
        kind: BundledKind::Workflow,
        name: "implement-and-review",
        files: &[
            (
                "implement-and-review/config.toml",
                include_str!("../../../assets/workflows/implement-and-review/config.toml"),
            ),
            (
                "implement-and-review/workflow.md",
                include_str!("../../../assets/workflows/implement-and-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-implement-review",
        files: &[
            (
                "research-implement-review/config.toml",
                include_str!("../../../assets/workflows/research-implement-review/config.toml"),
            ),
            (
                "research-implement-review/workflow.md",
                include_str!("../../../assets/workflows/research-implement-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "implement-qa-review",
        files: &[
            (
                "implement-qa-review/config.toml",
                include_str!("../../../assets/workflows/implement-qa-review/config.toml"),
            ),
            (
                "implement-qa-review/workflow.md",
                include_str!("../../../assets/workflows/implement-qa-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-implement-qa-review",
        files: &[
            (
                "research-implement-qa-review/config.toml",
                include_str!("../../../assets/workflows/research-implement-qa-review/config.toml"),
            ),
            (
                "research-implement-qa-review/workflow.md",
                include_str!("../../../assets/workflows/research-implement-qa-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-only",
        files: &[
            (
                "research-only/config.toml",
                include_str!("../../../assets/workflows/research-only/config.toml"),
            ),
            (
                "research-only/workflow.md",
                include_str!("../../../assets/workflows/research-only/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "solo",
        files: &[
            (
                "solo/config.toml",
                include_str!("../../../assets/workflows/solo/config.toml"),
            ),
            (
                "solo/workflow.md",
                include_str!("../../../assets/workflows/solo/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "pr-review",
        files: &[
            (
                "pr-review/config.toml",
                include_str!("../../../assets/workflows/pr-review/config.toml"),
            ),
            (
                "pr-review/workflow.md",
                include_str!("../../../assets/workflows/pr-review/workflow.md"),
            ),
        ],
    },
];

pub(super) fn items_of_kind(kind: BundledKind) -> impl Iterator<Item = &'static BundledItem> {
    BUNDLED_ITEMS.iter().filter(move |i| i.kind == kind)
}

/// Whether pm bundles an item of `kind` named `name`.
pub(crate) fn is_bundled(kind: BundledKind, name: &str) -> bool {
    items_of_kind(kind).any(|i| i.name == name)
}

/// Every bundled item's kind and name.
pub(crate) fn bundled_items() -> impl Iterator<Item = (BundledKind, &'static str)> {
    BUNDLED_ITEMS.iter().map(|i| (i.kind, i.name))
}

pub(super) fn find_item(kind: BundledKind, name: &str) -> Result<&'static BundledItem> {
    items_of_kind(kind)
        .find(|i| i.name == name)
        .ok_or_else(|| kind.not_found_error(name))
}

pub(super) fn is_installed(base_dir: &Path, item: &BundledItem) -> bool {
    item.files
        .iter()
        .all(|(rel, _)| base_dir.join(rel).exists())
}

pub(super) fn is_up_to_date(base_dir: &Path, item: &BundledItem) -> bool {
    item.files.iter().all(
        |(rel, content)| match fs::read_to_string(base_dir.join(rel)) {
            Ok(installed) => installed == *content,
            Err(_) => false,
        },
    )
}

/// Whether `rel` (relative to a store root, e.g. `agents/reviewer.md`) is a
/// file pm bundles.
pub(super) fn is_bundled_asset(rel: &Path) -> bool {
    BUNDLED_ITEMS.iter().any(|item| {
        item.kind.store_subdir().is_some_and(|sub| {
            item.files
                .iter()
                .any(|(file, _)| Path::new(sub).join(file) == rel)
        })
    })
}

/// Whether `name` is one of the bundled workflows — pm-owned in the global
/// tier, where `pm upgrade` rewrites them. A same-named project workflow is
/// a user override.
pub fn is_bundled_workflow(name: &str) -> bool {
    items_of_kind(BundledKind::Workflow).any(|i| i.name == name)
}

/// The bundled workflow names, sorted.
pub fn bundled_workflow_names() -> Vec<&'static str> {
    let mut names: Vec<_> = items_of_kind(BundledKind::Workflow)
        .map(|i| i.name)
        .collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::skills::{BASELINE_FILE, CANONICAL_DIR};

    #[test]
    fn bundled_workflow_names_are_reserved() {
        assert!(is_bundled_workflow("solo"));
        assert!(!is_bundled_workflow("my-solo"));
    }

    #[test]
    fn pms_own_gitignore_lists_every_bundled_project_copy() {
        let gitignore = include_str!("../../../.gitignore");
        let mut listed: Vec<String> = gitignore
            .split("# Copies of pm's own bundled assets")
            .nth(1)
            .expect("bundled-copies block")
            .lines()
            .skip(1)
            .take_while(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        listed.sort();

        let mut expected = vec![format!("/{CANONICAL_DIR}/{BASELINE_FILE}")];
        for item in items_of_kind(BundledKind::Agent) {
            expected.extend(
                item.files
                    .iter()
                    .map(|(file, _)| format!("/{CANONICAL_DIR}/agents/{file}")),
            );
        }
        expected.extend(
            items_of_kind(BundledKind::Skill)
                .map(|item| format!("/{CANONICAL_DIR}/skills/{}/", item.name)),
        );
        expected.sort();

        assert_eq!(listed, expected);
    }
}
