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
        name: "messaging",
        files: &[(
            "messaging/SKILL.md",
            include_str!("../../../bundled/skills/messaging/SKILL.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Skill,
        name: "pm-workflow",
        files: &[(
            "pm-workflow/SKILL.md",
            include_str!("../../../bundled/skills/pm-workflow/SKILL.md"),
        )],
    },
    // Agents
    BundledItem {
        kind: BundledKind::Agent,
        name: "reviewer",
        files: &[(
            "reviewer.md",
            include_str!("../../../bundled/agents/reviewer.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "implementer",
        files: &[(
            "implementer.md",
            include_str!("../../../bundled/agents/implementer.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "researcher",
        files: &[(
            "researcher.md",
            include_str!("../../../bundled/agents/researcher.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "qa",
        files: &[("qa.md", include_str!("../../../bundled/agents/qa.md"))],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: crate::state::scope::DEFAULT_ORCHESTRATOR,
        files: &[("main.md", include_str!("../../../bundled/agents/main.md"))],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: crate::commands::onboard::SUPERVISOR,
        files: &[(
            "supervisor.md",
            include_str!("../../../bundled/agents/supervisor.md"),
        )],
    },
    // Baseline (shared operating prompt appended to every spawned agent)
    BundledItem {
        kind: BundledKind::Baseline,
        name: "pm-baseline",
        files: &[(
            "pm-baseline.md",
            include_str!("../../../bundled/baseline/pm-baseline.md"),
        )],
    },
    // Workflows
    BundledItem {
        kind: BundledKind::Workflow,
        name: "implement-and-review",
        files: &[
            (
                "implement-and-review/config.toml",
                include_str!("../../../bundled/workflows/implement-and-review/config.toml"),
            ),
            (
                "implement-and-review/workflow.md",
                include_str!("../../../bundled/workflows/implement-and-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-implement-review",
        files: &[
            (
                "research-implement-review/config.toml",
                include_str!("../../../bundled/workflows/research-implement-review/config.toml"),
            ),
            (
                "research-implement-review/workflow.md",
                include_str!("../../../bundled/workflows/research-implement-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "implement-qa-review",
        files: &[
            (
                "implement-qa-review/config.toml",
                include_str!("../../../bundled/workflows/implement-qa-review/config.toml"),
            ),
            (
                "implement-qa-review/workflow.md",
                include_str!("../../../bundled/workflows/implement-qa-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-implement-qa-review",
        files: &[
            (
                "research-implement-qa-review/config.toml",
                include_str!("../../../bundled/workflows/research-implement-qa-review/config.toml"),
            ),
            (
                "research-implement-qa-review/workflow.md",
                include_str!("../../../bundled/workflows/research-implement-qa-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-only",
        files: &[
            (
                "research-only/config.toml",
                include_str!("../../../bundled/workflows/research-only/config.toml"),
            ),
            (
                "research-only/workflow.md",
                include_str!("../../../bundled/workflows/research-only/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "solo",
        files: &[
            (
                "solo/config.toml",
                include_str!("../../../bundled/workflows/solo/config.toml"),
            ),
            (
                "solo/workflow.md",
                include_str!("../../../bundled/workflows/solo/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "orchestrator",
        files: &[
            (
                "orchestrator/config.toml",
                include_str!("../../../bundled/workflows/orchestrator/config.toml"),
            ),
            (
                "orchestrator/workflow.md",
                include_str!("../../../bundled/workflows/orchestrator/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "pr-review",
        files: &[
            (
                "pr-review/config.toml",
                include_str!("../../../bundled/workflows/pr-review/config.toml"),
            ),
            (
                "pr-review/workflow.md",
                include_str!("../../../bundled/workflows/pr-review/workflow.md"),
            ),
        ],
    },
];

/// An item pm once bundled and no longer ships.
pub(super) struct RetiredItem {
    pub(super) kind: BundledKind,
    pub(super) name: &'static str,
    /// Each file it installed (relative to its kind's store dir), with the
    /// SHA-256 of every content pm ever shipped for it.
    pub(super) files: &'static [(&'static str, &'static [&'static str])],
}

/// Every global install deletes these from the global tier and its harness
/// projections, so they don't linger as global customs. Once retired the
/// name is free, so only a file whose content pm shipped is deleted; anything
/// else is the user's.
pub(super) const RETIRED_ITEMS: &[RetiredItem] = &[RetiredItem {
    kind: BundledKind::Skill,
    name: "pm",
    files: &[(
        "pm/SKILL.md",
        &[
            "48e2824bc215d481f1bb307d2e35a0a57235a2959c255f761b930211f8aa4643",
            "c972e03d62876cf588f856a7d1a93a068859f72c1c8c343b4f66f849a03d5fbe",
            "2382f1be86d833cc7f8a4e878598cb1f78e77aa15ebba10076fa536e32d00f8d",
            "847c373e6bb7b2babd785e7378a8b3934fe843ebfeee215d5e93002b283b88ea",
            "78b76dded044d3bdbf4c689aac21f86e9ca6ddcf388d3d8d8f4c26e01e02917c",
            "07594610c6b44686705c92d52916ddda4f5815bb6fc23580b2a3888048313992",
            "804a395a44b53081423b86505976a913193129cecb059cc32445dcfc78f13aad",
            "c2e918258267a12fbb3f21ea31cfe3c8d75d3878cb689789af56646686b00d1d",
            "73d744ef7346adabeedc1f9a7ec35b691bdd9bc921ab291fc4d8d4e45af5a9b3",
            "413af84def16824e6880f14b5d021857bb71a02371ae6488465c799d1c2922ae",
            "2cecd71d519b5b5de43d9349930cda5d0d04546a8b91388b88e9fda065d1edd3",
            "4842f62a6c99f7033fdae574c91befc1c5f5945803cea57216aa4cad0ee83265",
            "59d8540fd90cace46ec08d39016d3e280b3decf75e586913590b942adcc5fa2c",
            "58e5bd86f1b1de413e833ae27e7fcb8b5a4bcf35aa3f1b23058662229e2067df",
            "b08fe0211f153527622ea3a2a674f20e70131462a659b42c4b03a1ff56041f8c",
            "22060eddeeb492ce0f8e4037caac40ee328bc37f7ee747ce164b93ee1bbd837b",
            "07d03eb219a4b4bbc8a5eef017f2d134e4d5f2107e8632f57f1c8eb870000d1e",
            "788f4de272ca216b36f73436c66e929d6c8729488e238208fe34dbbb3dc4c567",
            "99379dcb7da58bd3c66e4a2db28ddbe21c4a78bf44c11e103c5dc7bdcb26df38",
        ],
    )],
}];

pub(super) fn items_of_kind(kind: BundledKind) -> impl Iterator<Item = &'static BundledItem> {
    BUNDLED_ITEMS.iter().filter(move |i| i.kind == kind)
}

pub(super) fn retired_of_kind(kind: BundledKind) -> impl Iterator<Item = &'static RetiredItem> {
    RETIRED_ITEMS.iter().filter(move |i| i.kind == kind)
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
/// tier, where `pm update` rewrites them. A same-named project workflow is
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
    fn no_retired_name_is_bundled() {
        for item in RETIRED_ITEMS {
            assert!(!is_bundled(item.kind, item.name), "{}", item.name);
        }
    }

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
