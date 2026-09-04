//! Bundled assets and the canonical store they install into.
//!
//! Skills, agent definitions, and the baseline live canonically under
//! `<base>/.agents/` (`main/` for a project, `~` for `--global`). No harness
//! reads pm's `.agents/agents/`, so after every install the store is
//! *projected* into each harness's own layout (`.claude/{agents,skills}` for
//! claude-code) via [`Harness::project_assets`]; the canonical copy always
//! wins over a same-named projected file, and projection never deletes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::copy_dir_recursive;
use crate::harness::{self, Harness};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};

/// The canonical asset store, relative to the main worktree or home.
pub const CANONICAL_DIR: &str = ".agents";

// --- Unified bundled item system ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundledKind {
    Skill,
    Agent,
    Workflow,
    /// Single shared "operating baseline" file appended to every spawned
    /// agent's system prompt.
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

    /// Subdirectory of the canonical store (and of every harness projection)
    /// this kind lives in; `None` for kinds that aren't projected.
    fn store_subdir(self) -> Option<&'static str> {
        match self {
            Self::Skill => Some("skills"),
            Self::Agent => Some("agents"),
            Self::Workflow | Self::Baseline => None,
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
        name: "solo",
        files: &[
            (
                "solo/config.toml",
                include_str!("../../workflows/solo/config.toml"),
            ),
            (
                "solo/workflow.md",
                include_str!("../../workflows/solo/workflow.md"),
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
    match kind.store_subdir() {
        Some(subdir) => {
            let home = dirs::home_dir().ok_or(PmError::NoHomeDir)?;
            Ok(Some(home.join(CANONICAL_DIR).join(subdir)))
        }
        // The baseline is project-only — it lives next to the project's
        // installed agents and is referenced by absolute path at spawn time.
        None => Ok(None),
    }
}

/// Return the project-level install directory for a bundled kind.
fn project_dir(project_root: &Path, kind: BundledKind) -> PathBuf {
    let canonical = paths::main_worktree(project_root).join(CANONICAL_DIR);
    match kind {
        BundledKind::Skill | BundledKind::Agent => canonical.join(kind.store_subdir().unwrap()),
        BundledKind::Baseline => canonical,
        BundledKind::Workflow => paths::workflows_dir(project_root),
    }
}

// --- Projection into harness layouts ---

/// The harnesses whose projections this project maintains: the default plus
/// any named in `[agents.harness]`. A missing or unreadable project config
/// contributes nothing (the default still applies).
pub fn harnesses_in_use(project_root: &Path) -> Vec<Harness> {
    let project = ProjectConfig::load(&paths::pm_dir(project_root))
        .map(|c| c.agents)
        .unwrap_or_default();
    harness::harnesses_in_use(&project, &GlobalConfig::load_or_default().agents)
}

/// Project the main worktree's canonical store into every harness in use.
/// Returns one line per harness whose projection changed (`Would project …`
/// in `dry_run`, which writes nothing); in sync yields no lines.
pub fn project_assets(project_root: &Path, dry_run: bool) -> Result<Vec<String>> {
    let harnesses = harnesses_in_use(project_root);
    project_from(&paths::main_worktree(project_root), &harnesses, dry_run)
}

/// Project the user's global canonical store (`~/.agents`) into every
/// supported harness's home layout. Only explicit `--global` installs reach
/// here — `pm upgrade` never touches the home directory.
pub fn project_assets_global() -> Result<Vec<String>> {
    let home = dirs::home_dir().ok_or(PmError::NoHomeDir)?;
    project_from(&home, Harness::SUPPORTED, false)
}

fn project_from(base: &Path, harnesses: &[Harness], dry_run: bool) -> Result<Vec<String>> {
    let canonical = base.join(CANONICAL_DIR);
    let mut lines = Vec::new();
    if !canonical.is_dir() {
        return Ok(lines);
    }
    for h in harnesses {
        let target = base.join(h.config_dir());
        let projection = h.project_assets(&canonical, &target, dry_run)?;
        if projection.is_empty() {
            continue;
        }
        let verb = if dry_run {
            "Would project"
        } else {
            "Projected"
        };
        let n = projection.written.len();
        let mut line = format!(
            "{verb} {n} file{} into {}/ for {h}",
            if n == 1 { "" } else { "s" },
            h.config_dir()
        );
        // A user-authored file under the harness dir losing to a same-named
        // canonical one is what the user asked for, but say so once.
        let collisions: Vec<String> = projection
            .replaced
            .iter()
            .filter(|rel| !is_bundled_asset(rel))
            .map(|rel| rel.display().to_string())
            .collect();
        if !collisions.is_empty() {
            line.push_str(&format!(
                " (canonical copy replaced: {})",
                collisions.join(", ")
            ));
        }
        lines.push(line);
    }
    Ok(lines)
}

/// Whether `rel` (relative to a store root, e.g. `agents/reviewer.md`) is a
/// file pm bundles.
fn is_bundled_asset(rel: &Path) -> bool {
    BUNDLED_ITEMS.iter().any(|item| {
        item.kind.store_subdir().is_some_and(|sub| {
            item.files
                .iter()
                .any(|(file, _)| Path::new(sub).join(file) == rel)
        })
    })
}

/// Remove the projected copies of `kind`/`name` from every harness dir
/// under `base`. Only the explicit uninstall commands do this — projection
/// itself never deletes.
fn uninstall_projected(
    base: &Path,
    harnesses: &[Harness],
    kind: BundledKind,
    name: Option<&str>,
) -> Result<()> {
    let Some(subdir) = kind.store_subdir() else {
        return Ok(());
    };
    for h in harnesses {
        let dir = base.join(h.config_dir()).join(subdir);
        if dir.is_dir() {
            uninstall_in(&dir, kind, name)?;
        }
    }
    Ok(())
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
    uninstall_global(BundledKind::Skill, name)
}

pub fn skills_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_project(project_root, BundledKind::Skill, name)
}

fn uninstall_global(kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let dir = global_dir(kind)?.ok_or(PmError::NoHomeDir)?;
    let messages = uninstall_in(&dir, kind, name)?;
    let home = dirs::home_dir().ok_or(PmError::NoHomeDir)?;
    uninstall_projected(&home, Harness::SUPPORTED, kind, name)?;
    Ok(messages)
}

fn uninstall_project(
    project_root: &Path,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<Vec<String>> {
    let messages = uninstall_in(&project_dir(project_root, kind), kind, name)?;
    uninstall_projected(
        &paths::main_worktree(project_root),
        &harnesses_in_use(project_root),
        kind,
        name,
    )?;
    Ok(messages)
}

/// Copy main's skills — the canonical store and each harness's projection —
/// into the feature worktree's matching directories.
pub fn skills_pull(project_root: &Path, feature_name: &str) -> Result<()> {
    super::claude_settings::require_feature(project_root, feature_name)?;

    let main = paths::main_worktree(project_root);
    let feature = project_root.join(feature_name);
    let mut rels = vec![PathBuf::from(CANONICAL_DIR).join("skills")];
    for h in harnesses_in_use(project_root) {
        rels.push(PathBuf::from(h.config_dir()).join("skills"));
    }
    let present: Vec<&PathBuf> = rels.iter().filter(|r| main.join(r).is_dir()).collect();
    if present.is_empty() {
        return Err(PmError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no skills directory in main at {}",
                main.join(&rels[0]).display()
            ),
        )));
    }
    for rel in present {
        copy_dir_recursive(&main.join(rel), &feature.join(rel))?;
    }
    Ok(())
}

// --- Public API: Agents ---

pub fn agents_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_both(BundledKind::Agent, project_root)
}

pub fn agents_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_global(BundledKind::Agent, name)
}

pub fn agents_uninstall_project(project_root: &Path, name: Option<&str>) -> Result<Vec<String>> {
    uninstall_project(project_root, BundledKind::Agent, name)
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

const BASELINE_FILE: &str = "pm-baseline.md";

/// Absolute path to the installed shared baseline
/// (`main/.agents/pm-baseline.md`). A project not yet upgraded to the
/// canonical store still has it at the legacy `main/.claude/` location, so
/// that is returned when only it exists; the caller checks existence either
/// way.
pub fn baseline_path(project_root: &Path) -> PathBuf {
    let canonical = project_dir(project_root, BundledKind::Baseline).join(BASELINE_FILE);
    if canonical.exists() {
        return canonical;
    }
    let legacy = legacy_baseline_path(project_root);
    if legacy.exists() { legacy } else { canonical }
}

/// Where releases before the canonical store installed the baseline.
pub fn legacy_baseline_path(project_root: &Path) -> PathBuf {
    paths::main_worktree(project_root)
        .join(Harness::ClaudeCode.config_dir())
        .join(BASELINE_FILE)
}

/// Whether the legacy baseline is due for removal: it exists alongside an
/// installed canonical one.
pub fn legacy_baseline_superseded(project_root: &Path) -> bool {
    project_dir(project_root, BundledKind::Baseline)
        .join(BASELINE_FILE)
        .exists()
        && legacy_baseline_path(project_root).exists()
}

/// Remove the legacy baseline once the canonical one is installed — the
/// one pm-owned file at a fixed path under `.claude/` that pm deletes.
/// Returns whether anything was removed.
pub fn remove_legacy_baseline(project_root: &Path) -> Result<bool> {
    if !legacy_baseline_superseded(project_root) {
        return Ok(false);
    }
    fs::remove_file(legacy_baseline_path(project_root))?;
    Ok(true)
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
    fn install_project_writes_to_canonical_store() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let messages = skills_install_project(project_root, Some("pm")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed"));

        let skill_path = paths::main_worktree(project_root)
            .join(".agents")
            .join("skills")
            .join("pm")
            .join("SKILL.md");
        assert!(skill_path.exists());
    }

    #[test]
    fn project_assets_overwrites_bundled_keeps_foreign_and_reports_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        let claude_agents = main.join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(claude_agents.join("reviewer.md"), "stale").unwrap();
        fs::write(claude_agents.join("mine.md"), "user's own").unwrap();
        fs::write(claude_agents.join("shared.md"), "harness copy").unwrap();

        // Nothing canonical yet: dry-run and real projection are both no-ops.
        assert!(project_assets(project_root, true).unwrap().is_empty());
        assert!(project_assets(project_root, false).unwrap().is_empty());

        agents_install_project(project_root, None).unwrap();
        // A user-authored canonical def colliding with a harness-dir one.
        fs::write(main.join(".agents/agents/shared.md"), "canonical").unwrap();

        let dry = project_assets(project_root, true).unwrap();
        assert_eq!(dry.len(), 1, "{dry:?}");
        assert!(dry[0].starts_with("Would project"), "{}", dry[0]);
        assert_eq!(
            fs::read_to_string(claude_agents.join("reviewer.md")).unwrap(),
            "stale"
        );

        let lines = project_assets(project_root, false).unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("for claude-code"), "{}", lines[0]);
        assert!(
            lines[0].contains("replaced: agents/shared.md") && !lines[0].contains("reviewer.md"),
            "{}",
            lines[0]
        );
        let bundled = items_of_kind(BundledKind::Agent)
            .find(|i| i.name == "reviewer")
            .unwrap();
        assert_eq!(
            fs::read_to_string(claude_agents.join("reviewer.md")).unwrap(),
            bundled.files[0].1
        );
        assert_eq!(
            fs::read_to_string(claude_agents.join("mine.md")).unwrap(),
            "user's own"
        );
        assert_eq!(
            fs::read_to_string(claude_agents.join("shared.md")).unwrap(),
            "canonical"
        );

        // In sync: the dry-run is empty again.
        assert!(project_assets(project_root, true).unwrap().is_empty());
    }

    #[test]
    fn global_layout_projects_to_every_supported_harness_and_uninstalls_projections() {
        // `project_assets_global` / `uninstall_global` run over the real
        // home; exercise the same fan-out over a temp base.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        install_in(
            &base.join(".agents/agents"),
            BundledKind::Agent,
            Some("reviewer"),
        )
        .unwrap();

        let lines = project_from(base, Harness::SUPPORTED, false).unwrap();
        assert_eq!(lines.len(), Harness::SUPPORTED.len(), "{lines:?}");
        for h in Harness::SUPPORTED {
            assert!(
                base.join(h.config_dir())
                    .join("agents/reviewer.md")
                    .exists(),
                "{h}"
            );
        }

        uninstall_projected(
            base,
            Harness::SUPPORTED,
            BundledKind::Agent,
            Some("reviewer"),
        )
        .unwrap();
        for h in Harness::SUPPORTED {
            assert!(
                !base
                    .join(h.config_dir())
                    .join("agents/reviewer.md")
                    .exists()
            );
        }
        // Only the projections go; the canonical file is the caller's job.
        assert!(base.join(".agents/agents/reviewer.md").exists());
    }

    #[test]
    fn uninstall_project_removes_projected_copy_too() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();
        agents_install_project(project_root, Some("reviewer")).unwrap();
        project_assets(project_root, false).unwrap();
        let main = paths::main_worktree(project_root);
        assert!(main.join(".claude/agents/reviewer.md").exists());

        agents_uninstall_project(project_root, Some("reviewer")).unwrap();
        assert!(!main.join(".agents/agents/reviewer.md").exists());
        assert!(!main.join(".claude/agents/reviewer.md").exists());
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
    fn pull_copies_canonical_skills_when_main_has_only_that_store() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();

        let features_dir = project_root.join(".pm").join("features");
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(features_dir.join("my-feat.toml"), "branch = \"my-feat\"\n").unwrap();

        let canonical = paths::main_worktree(project_root).join(".agents/skills/foo");
        fs::create_dir_all(&canonical).unwrap();
        fs::write(canonical.join("SKILL.md"), "canonical skill").unwrap();
        fs::create_dir_all(project_root.join("my-feat")).unwrap();

        skills_pull(project_root, "my-feat").unwrap();

        let feature = project_root.join("my-feat");
        assert_eq!(
            fs::read_to_string(feature.join(".agents/skills/foo/SKILL.md")).unwrap(),
            "canonical skill"
        );
        assert!(!feature.join(".claude").exists());
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
    fn agents_install_project_writes_to_canonical_store() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let messages = agents_install_project(project_root, Some("reviewer")).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed"));

        let agent_path = paths::main_worktree(project_root)
            .join(".agents")
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
    fn workflows_install_all_installs_five() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let messages = workflows_install_project(project_root, None).unwrap();
        // Five bundled workflows
        assert_eq!(messages.len(), 5);
        for name in &[
            "implement-and-review",
            "research-implement-review",
            "research-only",
            "solo",
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
    fn baseline_install_writes_to_canonical_store() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();

        let messages = baseline_install_project(project_root, None).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Installed Baseline 'pm-baseline'"));
        assert_eq!(
            baseline_path(project_root),
            paths::main_worktree(project_root).join(".agents/pm-baseline.md")
        );
        assert!(baseline_path(project_root).exists());
    }

    #[test]
    fn baseline_path_prefers_canonical_and_falls_back_to_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        let canonical = main.join(".agents/pm-baseline.md");
        let legacy = main.join(".claude/pm-baseline.md");

        // Neither installed: the canonical path, for callers to test.
        assert_eq!(baseline_path(project_root), canonical);
        assert!(!baseline_path(project_root).exists());

        // Only the legacy file (project not yet upgraded): still applied.
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, "old").unwrap();
        assert_eq!(baseline_path(project_root), legacy);
        assert!(!legacy_baseline_superseded(project_root));
        assert!(!remove_legacy_baseline(project_root).unwrap());
        assert!(legacy.exists());

        // Both: canonical wins and the legacy one is due for removal.
        baseline_install_project(project_root, None).unwrap();
        assert_eq!(baseline_path(project_root), canonical);
        assert!(legacy_baseline_superseded(project_root));
        assert!(remove_legacy_baseline(project_root).unwrap());
        assert!(!legacy.exists());
        assert!(!remove_legacy_baseline(project_root).unwrap());
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
