use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::copy_dir_recursive;
use crate::state::paths;

// --- Unified bundled item system ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundledKind {
    Skill,
    Agent,
    Workflow,
    /// Single shared "operating baseline" file appended to every spawned
    /// agent's system prompt via `claude --append-system-prompt-file`.
    Baseline,
}

/// How `install_in` treats an already-installed item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallPolicy {
    /// Bundle is authoritative: outdated installs are rewritten on
    /// upgrade. Used for skills and agents — pm controls their content.
    Overwrite,
    /// User edits are preserved: an already-installed item is skipped on
    /// upgrade, even if its content drifted from the bundled version.
    /// Used for workflows, where the bundle is just a starter template.
    Preserve,
}

impl BundledKind {
    fn label(self) -> &'static str {
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

    fn install_policy(self) -> InstallPolicy {
        match self {
            Self::Skill | Self::Agent | Self::Baseline => InstallPolicy::Overwrite,
            Self::Workflow => InstallPolicy::Preserve,
        }
    }
}

struct BundledItem {
    kind: BundledKind,
    name: &'static str,
    /// One or more files that make up this item, relative to the install
    /// directory. Skills and agents have exactly one file each; workflows
    /// have two (`config.toml` + `workflow.md`).
    files: &'static [(&'static str, &'static str)],
}

const BUNDLED_ITEMS: &[BundledItem] = &[
    // Skills
    BundledItem {
        kind: BundledKind::Skill,
        name: "pm",
        files: &[("pm/SKILL.md", include_str!("../../skills/pm/SKILL.md"))],
    },
    BundledItem {
        kind: BundledKind::Skill,
        name: "messaging",
        files: &[(
            "messaging/SKILL.md",
            include_str!("../../skills/messaging/SKILL.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Skill,
        name: "pm-workflow",
        files: &[(
            "pm-workflow/SKILL.md",
            include_str!("../../skills/pm-workflow/SKILL.md"),
        )],
    },
    // Agents
    BundledItem {
        kind: BundledKind::Agent,
        name: "reviewer",
        files: &[("reviewer.md", include_str!("../../agents/reviewer.md"))],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "implementer",
        files: &[(
            "implementer.md",
            include_str!("../../agents/implementer.md"),
        )],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "researcher",
        files: &[("researcher.md", include_str!("../../agents/researcher.md"))],
    },
    BundledItem {
        kind: BundledKind::Agent,
        name: "main",
        files: &[("main.md", include_str!("../../agents/main.md"))],
    },
    // Baseline (shared operating prompt appended to every spawned agent)
    BundledItem {
        kind: BundledKind::Baseline,
        name: "pm-baseline",
        files: &[(
            "pm-baseline.md",
            include_str!("../../baseline/pm-baseline.md"),
        )],
    },
    // Workflows
    BundledItem {
        kind: BundledKind::Workflow,
        name: "implement-and-review",
        files: &[
            (
                "implement-and-review/config.toml",
                include_str!("../../workflows/implement-and-review/config.toml"),
            ),
            (
                "implement-and-review/workflow.md",
                include_str!("../../workflows/implement-and-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-implement-review",
        files: &[
            (
                "research-implement-review/config.toml",
                include_str!("../../workflows/research-implement-review/config.toml"),
            ),
            (
                "research-implement-review/workflow.md",
                include_str!("../../workflows/research-implement-review/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "research-only",
        files: &[
            (
                "research-only/config.toml",
                include_str!("../../workflows/research-only/config.toml"),
            ),
            (
                "research-only/workflow.md",
                include_str!("../../workflows/research-only/workflow.md"),
            ),
        ],
    },
    BundledItem {
        kind: BundledKind::Workflow,
        name: "pr-review",
        files: &[
            (
                "pr-review/config.toml",
                include_str!("../../workflows/pr-review/config.toml"),
            ),
            (
                "pr-review/workflow.md",
                include_str!("../../workflows/pr-review/workflow.md"),
            ),
        ],
    },
];

fn items_of_kind(kind: BundledKind) -> impl Iterator<Item = &'static BundledItem> {
    BUNDLED_ITEMS.iter().filter(move |i| i.kind == kind)
}

fn is_installed(base_dir: &Path, item: &BundledItem) -> bool {
    item.files
        .iter()
        .all(|(rel, _)| base_dir.join(rel).exists())
}

fn is_up_to_date(base_dir: &Path, item: &BundledItem) -> bool {
    item.files.iter().all(
        |(rel, content)| match fs::read_to_string(base_dir.join(rel)) {
            Ok(installed) => installed == *content,
            Err(_) => false,
        },
    )
}

/// Return the global install directory for a bundled kind, or `None` if
/// the kind has no global install location (workflows are project-only).
fn global_dir(kind: BundledKind) -> Result<Option<PathBuf>> {
    match kind {
        BundledKind::Skill | BundledKind::Agent => {
            let home = dirs::home_dir().ok_or(PmError::NoHomeDir)?;
            let subdir = if kind == BundledKind::Skill {
                "skills"
            } else {
                "agents"
            };
            Ok(Some(home.join(".claude").join(subdir)))
        }
        // The baseline is project-only — it lives next to the project's
        // installed agents and is referenced by absolute path at spawn time.
        BundledKind::Workflow | BundledKind::Baseline => Ok(None),
    }
}

/// Return the project-level install directory for a bundled kind.
fn project_dir(project_root: &Path, kind: BundledKind) -> PathBuf {
    match kind {
        BundledKind::Skill => paths::main_worktree(project_root)
            .join(".claude")
            .join("skills"),
        BundledKind::Agent => paths::main_worktree(project_root)
            .join(".claude")
            .join("agents"),
        // Installed alongside agents at `main/.claude/pm-baseline.md`.
        BundledKind::Baseline => paths::main_worktree(project_root).join(".claude"),
        BundledKind::Workflow => paths::workflows_dir(project_root),
    }
}

fn status_label(dir: &Path, item: &BundledItem) -> &'static str {
    if !is_installed(dir, item) {
        "not installed"
    } else if is_up_to_date(dir, item) {
        "installed"
    } else {
        "outdated"
    }
}

fn list_both(kind: BundledKind, project_root: Option<&Path>) -> Result<Vec<String>> {
    let global = global_dir(kind)?;
    let project = project_root.map(|r| project_dir(r, kind));

    let mut lines = Vec::new();
    for item in items_of_kind(kind) {
        let global_status = global.as_deref().map(|g| status_label(g, item));

        match (&project, global_status) {
            (Some(proj), Some(gs)) => {
                let ps = status_label(proj, item);
                lines.push(format!("  {} — project: {}, global: {}", item.name, ps, gs));
            }
            (Some(proj), None) => {
                let ps = status_label(proj, item);
                lines.push(format!("  {} — {}", item.name, ps));
            }
            (None, Some(gs)) => {
                lines.push(format!("  {} — {}", item.name, gs));
            }
            (None, None) => {
                lines.push(format!("  {} — (no install location)", item.name));
            }
        }
    }
    Ok(lines)
}

fn install_in(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    install_in_with_policy(dir, kind, name, kind.install_policy())
}

/// Like [`install_in`] but lets the caller force an `Overwrite` policy
/// regardless of the kind's default. Used by explicit `pm workflow
/// install` calls so users can revert a hand-edited workflow back to
/// the bundled copy without `rm -rf`-ing the directory first.
fn install_in_with_policy(
    dir: &Path,
    kind: BundledKind,
    name: Option<&str>,
    policy: InstallPolicy,
) -> Result<Vec<String>> {
    let to_install = items_to_install(kind, name)?;

    let label = kind.label();
    let mut messages = Vec::new();
    for item in to_install {
        if is_up_to_date(dir, item) {
            messages.push(format!("{label} '{}' is already up to date", item.name));
            continue;
        }
        // Preserve policy is enforced per-file, not per-item: any
        // already-on-disk file is left alone (it may be user-modified),
        // but missing sibling files are still written. This guarantees
        // that editing one file in a multi-file item never causes a
        // sibling deletion + upgrade to silently restore the bundle.
        let mut wrote_any = false;
        let mut preserved_any = false;
        for (rel, content) in item.files {
            let path = dir.join(rel);
            if policy == InstallPolicy::Preserve && path.exists() {
                preserved_any = true;
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content)?;
            wrote_any = true;
        }
        let msg = match (wrote_any, preserved_any) {
            (true, true) => {
                format!(
                    "Installed {label} '{}' (partial — preserved user-modified files)",
                    item.name
                )
            }
            (true, false) => format!("Installed {label} '{}'", item.name),
            (false, true) => format!(
                "{label} '{}' is installed (user-modified, preserving)",
                item.name
            ),
            // Item was not up to date but had no files? Shouldn't happen
            // with the BUNDLED_ITEMS shape — every item has ≥ 1 file.
            (false, false) => format!("{label} '{}' had no files to install", item.name),
        };
        messages.push(msg);
    }
    Ok(messages)
}

/// Dry-run companion to [`install_in`]: returns one `Would …` line per item
/// whose on-disk content does not match the bundled content. Items that are
/// already up to date produce no output, keeping the contract simple — every
/// returned line corresponds to an action that would be taken.
fn install_in_dry_run(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let to_install = items_to_install(kind, name)?;
    let policy = kind.install_policy();

    let label = kind.label();
    let mut messages = Vec::new();
    for item in to_install {
        if is_up_to_date(dir, item) {
            continue;
        }
        // Match `install_in`'s per-file Preserve semantics: a file is
        // "to-be-installed" only when missing (for Preserve) or
        // missing-or-drifted (for Overwrite).
        let would_write_any = item.files.iter().any(|(rel, content)| {
            let path = dir.join(rel);
            match policy {
                InstallPolicy::Preserve => !path.exists(),
                InstallPolicy::Overwrite => match fs::read_to_string(&path) {
                    Ok(installed) => installed != *content,
                    Err(_) => true,
                },
            }
        });
        if !would_write_any {
            continue;
        }
        let verb = if is_installed(dir, item) {
            "update"
        } else {
            "install"
        };
        messages.push(format!("Would {verb} {label} '{}'", item.name));
    }
    Ok(messages)
}

fn items_to_install(kind: BundledKind, name: Option<&str>) -> Result<Vec<&'static BundledItem>> {
    Ok(match name {
        Some(n) => {
            let item = items_of_kind(kind)
                .find(|i| i.name == n)
                .ok_or_else(|| kind.not_found_error(n))?;
            vec![item]
        }
        None => items_of_kind(kind).collect(),
    })
}

fn uninstall_in(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let to_uninstall: Vec<&BundledItem> = match name {
        Some(n) => {
            let item = items_of_kind(kind)
                .find(|i| i.name == n)
                .ok_or_else(|| kind.not_found_error(n))?;
            vec![item]
        }
        None => items_of_kind(kind).collect(),
    };

    let label = kind.label();
    let mut messages = Vec::new();
    for item in to_uninstall {
        if !is_installed(dir, item) {
            messages.push(format!("{label} '{}' is not installed", item.name));
            continue;
        }
        for (rel, _content) in item.files {
            let path = dir.join(rel);
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            // Clean up empty parent directory (for skills/workflows that use a
            // subdirectory). Stop short of removing `dir` itself.
            if let Some(parent) = path.parent()
                && parent != dir
                && parent.read_dir().is_ok_and(|mut d| d.next().is_none())
            {
                let _ = std::fs::remove_dir(parent);
            }
        }
        messages.push(format!("Uninstalled {label} '{}'", item.name));
    }
    Ok(messages)
}

// --- Public API: Skills ---

pub fn skills_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_both(BundledKind::Skill, project_root)
}

pub fn skills_install(name: Option<&str>) -> Result<Vec<String>> {
    let dir = global_dir(BundledKind::Skill)?.ok_or(PmError::NoHomeDir)?;
    install_in(&dir, BundledKind::Skill, name)
}

pub fn skills_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    install_in(
        &project_dir(project_root, BundledKind::Skill),
        BundledKind::Skill,
        name,
    )
}

/// Dry-run variant of [`skills_install_project`]. Returns one `Would …`
/// line per skill that would be installed or updated; up-to-date skills
/// produce no output.
pub fn skills_install_project_dry_run(
    project_root: &Path,
    name: Option<&str>,
) -> Result<Vec<String>> {
    install_in_dry_run(
        &project_dir(project_root, BundledKind::Skill),
        BundledKind::Skill,
        name,
    )
}

pub fn skills_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    let dir = global_dir(BundledKind::Skill)?.ok_or(PmError::NoHomeDir)?;
    uninstall_in(&dir, BundledKind::Skill, name)
}

pub fn skills_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(
        &project_dir(project_root, BundledKind::Skill),
        BundledKind::Skill,
        name,
    )
}

pub fn skills_pull(project_root: &Path, feature_name: &str) -> Result<()> {
    super::claude_settings::require_feature(project_root, feature_name)?;

    let src = project_dir(project_root, BundledKind::Skill);
    if !src.is_dir() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no .claude/skills/ directory in main at {}", src.display()),
        )));
    }

    let dst = project_root
        .join(feature_name)
        .join(".claude")
        .join("skills");
    copy_dir_recursive(&src, &dst)
}

// --- Public API: Agents ---

pub fn agents_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_both(BundledKind::Agent, project_root)
}

pub fn agents_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    let dir = global_dir(BundledKind::Agent)?.ok_or(PmError::NoHomeDir)?;
    uninstall_in(&dir, BundledKind::Agent, name)
}

pub fn agents_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(
        &project_dir(project_root, BundledKind::Agent),
        BundledKind::Agent,
        name,
    )
}

pub fn agents_install(name: Option<&str>) -> Result<Vec<String>> {
    let dir = global_dir(BundledKind::Agent)?.ok_or(PmError::NoHomeDir)?;
    install_in(&dir, BundledKind::Agent, name)
}

pub fn agents_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    install_in(
        &project_dir(project_root, BundledKind::Agent),
        BundledKind::Agent,
        name,
    )
}

/// Dry-run variant of [`agents_install_project`]. Returns one `Would …`
/// line per agent that would be installed or updated; up-to-date agents
/// produce no output.
pub fn agents_install_project_dry_run(
    project_root: &Path,
    name: Option<&str>,
) -> Result<Vec<String>> {
    install_in_dry_run(
        &project_dir(project_root, BundledKind::Agent),
        BundledKind::Agent,
        name,
    )
}

// --- Public API: Baseline ---

/// Absolute path to the installed shared baseline file
/// (`main/.claude/pm-baseline.md`). Used by `agent_spawn` to pass
/// `--append-system-prompt-file` when the file exists.
pub fn baseline_path(project_root: &Path) -> PathBuf {
    project_dir(project_root, BundledKind::Baseline).join("pm-baseline.md")
}

pub fn baseline_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    install_in(
        &project_dir(project_root, BundledKind::Baseline),
        BundledKind::Baseline,
        name,
    )
}

/// Dry-run variant of [`baseline_install_project`]. Returns one `Would …`
/// line if the baseline would be installed or updated; up-to-date produces
/// no output.
pub fn baseline_install_project_dry_run(
    project_root: &Path,
    name: Option<&str>,
) -> Result<Vec<String>> {
    install_in_dry_run(
        &project_dir(project_root, BundledKind::Baseline),
        BundledKind::Baseline,
        name,
    )
}

// --- Public API: Workflows ---

pub fn workflows_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_both(BundledKind::Workflow, project_root)
}

pub fn workflows_install_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    install_in(
        &project_dir(project_root, BundledKind::Workflow),
        BundledKind::Workflow,
        name,
    )
}

/// Force-install bundled workflows, overwriting any on-disk content.
/// Used by the explicit `pm workflow install` CLI subcommand so users
/// can revert a hand-edited workflow back to the bundled copy without
/// deleting the directory first. The default `pm upgrade` install path
/// continues to preserve user edits.
pub fn workflows_install_project_force(
    project_root: &Path,
    name: Option<&str>,
) -> Result<Vec<String>> {
    install_in_with_policy(
        &project_dir(project_root, BundledKind::Workflow),
        BundledKind::Workflow,
        name,
        InstallPolicy::Overwrite,
    )
}

pub fn workflows_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_in(
        &project_dir(project_root, BundledKind::Workflow),
        BundledKind::Workflow,
        name,
    )
}

/// Dry-run variant of [`workflows_install_project`]. Returns one `Would …`
/// line per workflow that would be installed or updated; up-to-date
/// workflows produce no output.
pub fn workflows_install_project_dry_run(
    project_root: &Path,
    name: Option<&str>,
) -> Result<Vec<String>> {
    install_in_dry_run(
        &project_dir(project_root, BundledKind::Workflow),
        BundledKind::Workflow,
        name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bundled agents must carry no `tools:` frontmatter line. On the
    /// `claude --agent` path, omitting `tools` inherits the full tool set
    /// (incl. `Skill`, so the bundled skills load); a restrictive list would
    /// silently re-introduce the dark-agent regression. Guard the invariant.
    #[test]
    fn bundled_agents_have_no_tools_allowlist() {
        for item in items_of_kind(BundledKind::Agent) {
            let body = item.files[0].1;
            assert!(
                !body.lines().any(|l| l.trim_start().starts_with("tools:")),
                "agent '{}' must not declare a `tools:` allowlist",
                item.name
            );
        }
    }

    fn baseline_body() -> &'static str {
        items_of_kind(BundledKind::Baseline)
            .next()
            .expect("baseline item exists")
            .files[0]
            .1
    }

    /// The shared baseline owns the environment/CWD guidance that stops the
    /// emergent `cd "$(git rev-parse …)"` habit — both the no-`cd` and
    /// no-`$(…)` rules. It reaches every agent via
    /// `--append-system-prompt-file`, so the feature defs no longer repeat it.
    #[test]
    fn baseline_carries_no_cd_no_subst_guidance() {
        let body = baseline_body();
        assert!(
            body.contains("Do NOT `cd`"),
            "baseline must state no-cd rule"
        );
        assert!(
            body.contains("$(…)"),
            "baseline must warn against `$(…)` command substitution"
        );
    }

    /// The baseline is general (valid for every agent including `main`), so
    /// it must not name `.pm` — that store belongs solely to `main`, whose
    /// own def owns the boundary.
    #[test]
    fn baseline_is_pm_free_and_main_owns_pm() {
        assert!(
            !baseline_body().contains(".pm"),
            "baseline must not mention `.pm` — it's general to all agents"
        );
        let main = items_of_kind(BundledKind::Agent)
            .find(|i| i.name == "main")
            .expect("main agent exists")
            .files[0]
            .1;
        assert!(
            main.contains("../.pm/") && main.contains("own"),
            "main.md must state `../.pm/` is its store to own"
        );
    }

    /// Terminal routing prose must say "report in your own session", not the
    /// bare "respond to the user" that the implementer used to operationalise
    /// as a `pm msg reply` back to `main`. Guard against regressing it.
    #[test]
    fn workflows_avoid_bare_respond_to_user() {
        for item in items_of_kind(BundledKind::Workflow) {
            let (_, body) = item
                .files
                .iter()
                .find(|(path, _)| path.ends_with("workflow.md"))
                .unwrap_or_else(|| panic!("workflow '{}' has no workflow.md", item.name));
            assert!(
                !body.contains("respond to the user"),
                "workflow '{}' still uses the ambiguous \"respond to the user\" phrasing",
                item.name
            );
        }
    }

    // --- Shared install/list tests (exercise the unified logic) ---

    #[test]
    fn status_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let item = items_of_kind(BundledKind::Skill).next().unwrap();
        assert_eq!(status_label(&dir, item), "not installed");
    }

    #[test]
    fn install_by_name_and_check_status() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("pm"));

        let item = items_of_kind(BundledKind::Skill)
            .find(|i| i.name == "pm")
            .unwrap();
        assert!(is_installed(&dir, item));
        assert!(is_up_to_date(&dir, item));
        assert_eq!(status_label(&dir, item), "installed");
    }

    #[test]
    fn install_all_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let count = items_of_kind(BundledKind::Skill).count();
        let messages = install_in(&dir, BundledKind::Skill, None).unwrap();
        assert_eq!(messages.len(), count);

        for item in items_of_kind(BundledKind::Skill) {
            assert!(is_installed(&dir, item));
        }
    }

    #[test]
    fn install_all_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let count = items_of_kind(BundledKind::Agent).count();
        let messages = install_in(&dir, BundledKind::Agent, None).unwrap();
        assert_eq!(messages.len(), count);

        for item in items_of_kind(BundledKind::Agent) {
            assert!(is_installed(&dir, item));
        }
    }

    #[test]
    fn install_nonexistent_skill_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let err = install_in(&dir, BundledKind::Skill, Some("nonexistent")).unwrap_err();
        assert!(matches!(err, PmError::SkillNotFound(_)));
    }

    #[test]
    fn install_nonexistent_agent_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let err = install_in(&dir, BundledKind::Agent, Some("nonexistent")).unwrap_err();
        assert!(matches!(err, PmError::AgentNotFound(_)));
    }

    #[test]
    fn install_nonexistent_workflow_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        let err = install_in(&dir, BundledKind::Workflow, Some("nope")).unwrap_err();
        assert!(matches!(err, PmError::WorkflowNotFound(_)));
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let first = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(first[0].contains("Installed"));

        let second = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(second[0].contains("already up to date"));
    }

    #[test]
    fn outdated_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        fs::write(dir.join("pm/SKILL.md"), "old content").unwrap();

        let item = items_of_kind(BundledKind::Skill)
            .find(|i| i.name == "pm")
            .unwrap();
        assert!(is_installed(&dir, item));
        assert!(!is_up_to_date(&dir, item));
        assert_eq!(status_label(&dir, item), "outdated");
    }

    #[test]
    fn agent_install_and_check_status() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(messages[0].contains("Installed Agent 'reviewer'"));

        let reviewer = items_of_kind(BundledKind::Agent)
            .find(|i| i.name == "reviewer")
            .unwrap();
        let implementer = items_of_kind(BundledKind::Agent)
            .find(|i| i.name == "implementer")
            .unwrap();
        assert_eq!(status_label(&dir, reviewer), "installed");
        assert_eq!(status_label(&dir, implementer), "not installed");
    }

    #[test]
    fn agent_install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        let second = install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(second[0].contains("already up to date"));
    }

    // --- Skills-specific tests ---

    #[test]
    fn install_messaging_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = install_in(&dir, BundledKind::Skill, Some("messaging")).unwrap();
        assert!(messages[0].contains("Installed"));
        assert!(dir.join("messaging/SKILL.md").exists());

        let content = fs::read_to_string(dir.join("messaging/SKILL.md")).unwrap();
        assert!(content.contains("pm msg read"));
        assert!(content.contains("pm msg send"));
    }

    #[test]
    fn install_project_writes_to_main_claude_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let messages = skills_install_project(project_root, Some("pm")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed"));

        let skill_path = paths::main_worktree(project_root)
            .join(".claude")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        assert!(skill_path.exists());
    }

    #[test]
    fn pull_copies_skills_from_main_to_feature() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        let main_skills = paths::main_worktree(project_root)
            .join(".claude")
            .join("skills");
        fs::create_dir_all(main_skills.join("foo")).unwrap();
        fs::write(main_skills.join("foo").join("SKILL.md"), "skill content").unwrap();

        let feature_dir = project_root.join("my-feat");
        fs::create_dir_all(&feature_dir).unwrap();

        skills_pull(project_root, "my-feat").unwrap();

        let dst = feature_dir
            .join(".claude")
            .join("skills")
            .join("foo")
            .join("SKILL.md");
        assert!(dst.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "skill content");
    }

    #[test]
    fn pull_overwrites_existing_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        let main_skills = paths::main_worktree(project_root)
            .join(".claude")
            .join("skills");
        fs::create_dir_all(main_skills.join("foo")).unwrap();
        fs::write(main_skills.join("foo").join("SKILL.md"), "updated content").unwrap();

        let feature_skills = project_root
            .join("my-feat")
            .join(".claude")
            .join("skills")
            .join("foo");
        fs::create_dir_all(&feature_skills).unwrap();
        fs::write(feature_skills.join("SKILL.md"), "old content").unwrap();

        skills_pull(project_root, "my-feat").unwrap();

        let dst = feature_skills.join("SKILL.md");
        assert_eq!(fs::read_to_string(&dst).unwrap(), "updated content");
    }

    #[test]
    fn pull_errors_when_no_main_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();
        fs::create_dir_all(project_root.join("my-feat")).unwrap();

        let err = skills_pull(project_root, "my-feat").unwrap_err();
        assert!(matches!(err, PmError::Io(_)));
    }

    #[test]
    fn pull_errors_when_feature_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(project_root.join(".pm").join("features")).unwrap();

        let err = skills_pull(project_root, "nonexistent").unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(_)));
    }

    // --- Uninstall tests ---

    #[test]
    fn uninstall_removes_skill_file_and_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(dir.join("pm/SKILL.md").exists());

        let messages = uninstall_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(messages[0].contains("Uninstalled Skill 'pm'"));
        assert!(!dir.join("pm/SKILL.md").exists());
        // Parent dir (pm/) should be cleaned up since it's now empty
        assert!(!dir.join("pm").exists());
    }

    #[test]
    fn uninstall_removes_agent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(dir.join("reviewer.md").exists());

        let messages = uninstall_in(&dir, BundledKind::Agent, Some("reviewer")).unwrap();
        assert!(messages[0].contains("Uninstalled Agent 'reviewer'"));
        assert!(!dir.join("reviewer.md").exists());
    }

    #[test]
    fn uninstall_not_installed_returns_message() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        let messages = uninstall_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(messages[0].contains("is not installed"));
    }

    #[test]
    fn uninstall_all_removes_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");

        install_in(&dir, BundledKind::Agent, None).unwrap();
        let messages = uninstall_in(&dir, BundledKind::Agent, None).unwrap();

        for item in items_of_kind(BundledKind::Agent) {
            assert!(!is_installed(&dir, item));
        }
        assert!(messages.iter().all(|m| m.contains("Uninstalled")));
    }

    // --- Agents-specific tests ---

    #[test]
    fn agents_install_project_writes_to_main_claude_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let messages = agents_install_project(project_root, Some("reviewer")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed"));

        let agent_path = paths::main_worktree(project_root)
            .join(".claude")
            .join("agents")
            .join("reviewer.md");
        assert!(agent_path.exists());
    }

    // --- Workflows-specific tests ---

    #[test]
    fn workflows_install_writes_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let messages =
            workflows_install_project(project_root, Some("implement-and-review")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed Workflow 'implement-and-review'"));

        let dir = paths::workflows_dir(project_root).join("implement-and-review");
        assert!(dir.join("config.toml").exists());
        assert!(dir.join("workflow.md").exists());
    }

    #[test]
    fn workflows_install_all_installs_four() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let messages = workflows_install_project(project_root, None).unwrap();
        // Four bundled workflows
        assert_eq!(messages.len(), 4);
        for name in &[
            "implement-and-review",
            "research-implement-review",
            "research-only",
            "pr-review",
        ] {
            let dir = paths::workflows_dir(project_root).join(name);
            assert!(dir.join("config.toml").exists());
            assert!(dir.join("workflow.md").exists());
        }
    }

    #[test]
    fn workflows_install_preserves_user_edits() {
        // Workflows use the `Preserve` install policy: once a file is on
        // disk, `install_in` reports it as user-modified and refuses to
        // overwrite. This matches the policy the brief asks for and the
        // user-facing `pm upgrade` workflow test.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        workflows_install_project(project_root, Some("pr-review")).unwrap();

        let second = workflows_install_project(project_root, Some("pr-review")).unwrap();
        assert!(
            second[0].contains("already up to date"),
            "expected 'already up to date' on idempotent install, got: {second:?}"
        );

        // User edits a workflow file
        let wf_md = paths::workflows_dir(project_root)
            .join("pr-review")
            .join("workflow.md");
        fs::write(&wf_md, "user edits").unwrap();

        let third = workflows_install_project(project_root, Some("pr-review")).unwrap();
        assert!(
            third[0].contains("user-modified"),
            "expected 'user-modified' message, got: {third:?}"
        );
        assert_eq!(fs::read_to_string(&wf_md).unwrap(), "user edits");

        // Dry-run also reports nothing — user-modified workflow is skipped.
        let dry = workflows_install_project_dry_run(project_root, Some("pr-review")).unwrap();
        assert!(
            dry.is_empty(),
            "expected no dry-run actions when user-modified, got: {dry:?}"
        );
    }

    #[test]
    fn workflows_install_preserves_per_file_not_per_item() {
        // Regression: if a user modifies `config.toml` and deletes
        // `workflow.md`, `pm upgrade` must NOT overwrite the modified
        // `config.toml` even though one sibling file is missing. The
        // missing sibling should still be written.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        workflows_install_project(project_root, Some("pr-review")).unwrap();

        let wf_dir = paths::workflows_dir(project_root).join("pr-review");
        let cfg = wf_dir.join("config.toml");
        let md = wf_dir.join("workflow.md");

        // User edits config.toml…
        fs::write(&cfg, "user-edited config\n").unwrap();
        // …and deletes workflow.md.
        fs::remove_file(&md).unwrap();
        assert!(cfg.exists());
        assert!(!md.exists());

        let messages = workflows_install_project(project_root, Some("pr-review")).unwrap();
        // The user's config.toml must survive.
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "user-edited config\n");
        // The missing workflow.md must be restored to the bundled content.
        assert!(md.exists());
        // Message reflects the partial install.
        assert!(
            messages[0].contains("partial") || messages[0].contains("Installed"),
            "expected partial-install message, got: {messages:?}"
        );
    }

    // --- Baseline-specific tests ---

    #[test]
    fn baseline_install_writes_to_main_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let messages = baseline_install_project(project_root, None).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed Baseline 'pm-baseline'"));
        assert!(baseline_path(project_root).exists());
    }

    #[test]
    fn baseline_install_overwrites_user_edits() {
        // Baseline uses the Overwrite policy: pm controls its content.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        baseline_install_project(project_root, None).unwrap();
        fs::write(baseline_path(project_root), "stale").unwrap();

        let second = baseline_install_project(project_root, None).unwrap();
        assert!(second[0].contains("Installed Baseline"));
        let content = fs::read_to_string(baseline_path(project_root)).unwrap();
        assert!(content.contains("Operating baseline"));
    }

    #[test]
    fn baseline_install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        baseline_install_project(project_root, None).unwrap();
        let second = baseline_install_project(project_root, None).unwrap();
        assert!(second[0].contains("already up to date"));
    }

    #[test]
    fn baseline_dry_run_reports_install_then_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let dry = baseline_install_project_dry_run(project_root, None).unwrap();
        assert_eq!(dry.len(), 1);
        assert!(dry[0].contains("Would install Baseline 'pm-baseline'"));

        baseline_install_project(project_root, None).unwrap();
        let after = baseline_install_project_dry_run(project_root, None).unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn workflows_list_no_project_falls_back_to_no_install_location() {
        // Workflows have no global install location; listing without a
        // project_root should produce sentinel "(no install location)".
        let lines = workflows_list(None).unwrap();
        for line in &lines {
            assert!(line.contains("(no install location)"), "line: {line}");
        }
    }
}
