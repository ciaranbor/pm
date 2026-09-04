//! Bundled assets and the tiered store they install into.
//!
//! Every bundled kind installs once into the **global tier**: skills, agent
//! definitions, and the baseline under `~/.agents/`, workflows under the pm
//! config dir. `pm init`/`pm upgrade` refresh it and the bundle is
//! authoritative there (bundled names are reserved). The **project tier**
//! (`main/.agents/{agents,skills}`, `.pm/workflows/`) holds only the user's
//! customs and shadows the global tier by name. No harness reads pm's
//! `.agents/agents/`, so each tier's store is *projected* into the harness's
//! own layout (`~/.claude/` and `main/.claude/` for claude-code) via
//! [`Harness::project_assets`]; the canonical copy always wins over a
//! same-named projected file, and projection never deletes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PmError, Result};
use crate::fs_utils::{copy_dir_recursive, write_atomic};
use crate::harness::{self, Harness};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};

/// The canonical asset store, relative to the main worktree or home.
pub const CANONICAL_DIR: &str = ".agents";

const BASELINE_FILE: &str = "pm-baseline.md";

/// Marker file under `.pm/migrations/` recording that the project's bundled
/// copies were removed in favour of the global tier.
const MIGRATION_MARKER: &str = "global-assets";

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

impl BundledKind {
    const ALL: [BundledKind; 4] = [Self::Skill, Self::Agent, Self::Baseline, Self::Workflow];

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

fn find_item(kind: BundledKind, name: &str) -> Result<&'static BundledItem> {
    items_of_kind(kind)
        .find(|i| i.name == name)
        .ok_or_else(|| kind.not_found_error(name))
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

// --- Tiers ---

/// The global tier's on-disk locations. Production code resolves it from
/// the process environment; tests that mutate the tier build one over a
/// private tempdir with [`GlobalStore::at`].
pub struct GlobalStore {
    pub home: PathBuf,
    pub config_dir: PathBuf,
}

impl GlobalStore {
    pub fn resolve() -> Result<Self> {
        Ok(Self {
            home: paths::home_dir()?,
            config_dir: paths::global_config_dir()?,
        })
    }

    /// A store rooted entirely under `home`, with the config dir at
    /// `<home>/.config/pm` (also the layout `cfg(test)` resolution uses).
    pub fn at(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            config_dir: home.join(".config").join("pm"),
        }
    }

    fn canonical(&self) -> PathBuf {
        self.home.join(CANONICAL_DIR)
    }

    pub fn workflows_dir(&self) -> PathBuf {
        paths::global_workflows_dir_in(&self.config_dir)
    }

    pub fn baseline_path(&self) -> PathBuf {
        self.canonical().join(BASELINE_FILE)
    }

    fn dir(&self, kind: BundledKind) -> PathBuf {
        match kind {
            BundledKind::Skill | BundledKind::Agent => {
                self.canonical().join(kind.store_subdir().unwrap())
            }
            BundledKind::Baseline => self.canonical(),
            BundledKind::Workflow => self.workflows_dir(),
        }
    }
}

/// Where a bundled kind would live in the project tier. After migration
/// only user customs are found here.
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

/// Project the main worktree's canonical store (the project's customs) into
/// every harness in use. Returns one line per harness whose projection
/// changed (`Would project …` in `dry_run`, which writes nothing); in sync
/// yields no lines.
pub fn project_assets(project_root: &Path, dry_run: bool) -> Result<Vec<String>> {
    let main = paths::main_worktree(project_root);
    let canonical = main.join(CANONICAL_DIR);
    let mut lines = Vec::new();
    for h in harnesses_in_use(project_root) {
        let target = main.join(h.config_dir());
        lines.extend(project_into(&canonical, h, &target, dry_run)?);
    }
    Ok(lines)
}

/// Project the global canonical store into every supported harness's own
/// global dir (`~/.agents` → `~/.claude` for claude-code).
fn project_global(store: &GlobalStore, dry_run: bool) -> Result<Vec<String>> {
    let canonical = store.canonical();
    let mut lines = Vec::new();
    for h in Harness::SUPPORTED {
        let Some(target) = h.global_config_dir(&store.home) else {
            continue;
        };
        lines.extend(project_into(&canonical, *h, &target, dry_run)?);
    }
    Ok(lines)
}

fn project_into(
    canonical: &Path,
    h: Harness,
    target: &Path,
    dry_run: bool,
) -> Result<Option<String>> {
    if !canonical.is_dir() {
        return Ok(None);
    }
    let projection = h.project_assets(canonical, target, dry_run)?;
    if projection.is_empty() {
        return Ok(None);
    }
    let verb = if dry_run {
        "Would project"
    } else {
        "Projected"
    };
    let n = projection.written.len();
    let mut line = format!(
        "{verb} {n} file{} into {} for {h}",
        if n == 1 { "" } else { "s" },
        target.display()
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
    Ok(Some(line))
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

/// Remove the projected copies of `kind`/`name` from every supported
/// harness's global dir. Only the explicit uninstall commands do this —
/// projection itself never deletes.
fn uninstall_projected_global(
    store: &GlobalStore,
    kind: BundledKind,
    name: Option<&str>,
) -> Result<()> {
    let Some(subdir) = kind.store_subdir() else {
        return Ok(());
    };
    for h in Harness::SUPPORTED {
        let Some(target) = h.global_config_dir(&store.home) else {
            continue;
        };
        let dir = target.join(subdir);
        if dir.is_dir() {
            uninstall_in(&dir, kind, name)?;
        }
    }
    Ok(())
}

// --- Install / uninstall primitives ---

fn status_label(dir: &Path, item: &BundledItem) -> &'static str {
    if !is_installed(dir, item) {
        "not installed"
    } else if is_up_to_date(dir, item) {
        "installed"
    } else {
        "outdated"
    }
}

/// One line per bundled item of `kind`: its global status, and — inside a
/// project — whether a same-named project custom shadows it.
fn list_kind(kind: BundledKind, project_root: Option<&Path>) -> Result<Vec<String>> {
    let store = GlobalStore::resolve()?;
    let global = store.dir(kind);
    let mut lines = Vec::new();
    for item in items_of_kind(kind) {
        let mut line = format!("  {} — {}", item.name, status_label(&global, item));
        if project_root.is_some_and(|r| is_installed(&project_dir(r, kind), item)) {
            line.push_str(" (overridden by project custom)");
        }
        lines.push(line);
    }
    Ok(lines)
}

/// Install (or rewrite) bundled items of `kind` under `dir`. The bundle is
/// authoritative for every kind: an item whose on-disk content differs is
/// overwritten, and the message says which were rewritten. Paths under
/// `dir` that no bundled item names are never touched.
fn install_in(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let label = kind.label();
    let mut messages = Vec::new();
    for item in items_to_install(kind, name)? {
        if is_up_to_date(dir, item) {
            messages.push(format!("{label} '{}' is already up to date", item.name));
            continue;
        }
        // Rewriting drifted content is the one destructive step here, and pm
        // can't tell a user edit from a bundle change — so say which items
        // were rewritten rather than claiming why.
        let verb = if is_installed(dir, item) {
            "Rewrote"
        } else {
            "Installed"
        };
        for (rel, content) in item.files {
            write_atomic(&dir.join(rel), content.as_bytes())?;
        }
        messages.push(format!("{verb} {label} '{}'", item.name));
    }
    Ok(messages)
}

/// Dry-run companion to [`install_in`]: one `Would …` line per item whose
/// on-disk content does not match the bundle; up-to-date items produce no
/// output, so every returned line is an action that would be taken.
fn install_in_dry_run(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let label = kind.label();
    let mut messages = Vec::new();
    for item in items_to_install(kind, name)? {
        if is_up_to_date(dir, item) {
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
        Some(n) => vec![find_item(kind, n)?],
        None => items_of_kind(kind).collect(),
    })
}

fn uninstall_in(dir: &Path, kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let label = kind.label();
    let mut messages = Vec::new();
    for item in items_to_install(kind, name)? {
        if !is_installed(dir, item) {
            messages.push(format!("{label} '{}' is not installed", item.name));
            continue;
        }
        for (rel, _content) in item.files {
            let path = dir.join(rel);
            if path.exists() {
                fs::remove_file(&path)?;
            }
            prune_empty_parents(&path, dir);
        }
        messages.push(format!("Uninstalled {label} '{}'", item.name));
    }
    Ok(messages)
}

/// Remove now-empty directories between `path` and `stop` (exclusive).
fn prune_empty_parents(path: &Path, stop: &Path) {
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if dir == stop || !dir.starts_with(stop) {
            break;
        }
        if !dir.read_dir().is_ok_and(|mut d| d.next().is_none()) {
            break;
        }
        let _ = fs::remove_dir(dir);
        cur = dir.parent();
    }
}

// --- Global tier ---

/// Install every bundled kind into the global tier and project the
/// canonical store into each supported harness's global dir. Idempotent.
pub fn install_global() -> Result<Vec<String>> {
    install_global_in(&GlobalStore::resolve()?)
}

pub fn install_global_in(store: &GlobalStore) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for kind in BundledKind::ALL {
        for line in install_in(&store.dir(kind), kind, None)? {
            if !line.contains("already up to date") {
                lines.push(format!("{line} (global)"));
            }
        }
    }
    lines.extend(project_global(store, false)?);
    Ok(lines)
}

/// Dry-run of [`install_global`]: `Would …` lines only, nothing written.
pub fn install_global_dry_run() -> Result<Vec<String>> {
    install_global_dry_run_in(&GlobalStore::resolve()?)
}

pub fn install_global_dry_run_in(store: &GlobalStore) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for kind in BundledKind::ALL {
        for line in install_in_dry_run(&store.dir(kind), kind, None)? {
            lines.push(format!("{line} (global)"));
        }
    }
    lines.extend(project_global(store, true)?);
    Ok(lines)
}

/// Bundled items (as `Kind 'name'`) absent from the global tier.
pub fn global_store_missing() -> Result<Vec<String>> {
    Ok(global_store_missing_in(&GlobalStore::resolve()?))
}

pub fn global_store_missing_in(store: &GlobalStore) -> Vec<String> {
    let mut out = Vec::new();
    for kind in BundledKind::ALL {
        let dir = store.dir(kind);
        for item in items_of_kind(kind) {
            if !is_installed(&dir, item) {
                out.push(format!("{} '{}'", kind.label(), item.name));
            }
        }
    }
    out
}

/// Global agent definitions (`~/.agents/agents/*.md`) with no projected copy
/// in a supported harness's global definition dir.
pub fn unprojected_global_definitions() -> Result<Vec<(String, Harness)>> {
    unprojected_global_definitions_in(&GlobalStore::resolve()?)
}

pub fn unprojected_global_definitions_in(store: &GlobalStore) -> Result<Vec<(String, Harness)>> {
    let canonical = store.dir(BundledKind::Agent);
    let mut out = Vec::new();
    for file in definition_files(&canonical)? {
        for h in Harness::SUPPORTED {
            let projected = h
                .global_config_dir(&store.home)
                .map(|d| d.join("agents").join(&file));
            if projected.is_some_and(|p| !p.exists()) {
                out.push((file.trim_end_matches(".md").to_string(), *h));
            }
        }
    }
    Ok(out)
}

/// Sorted `*.md` filenames directly under `dir`; empty when it doesn't exist.
pub fn definition_files(dir: &Path) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| f.ends_with(".md"))
        .collect();
    names.sort();
    Ok(names)
}

/// Project custom skills (`main/.agents/skills/<name>`) that a harness
/// resolves the *global* same-named skill over, so the custom never takes
/// effect. Claude Code's personal-over-project precedence is the one place
/// project-shadows-global can't be delivered by placement.
pub fn shadowed_project_skills(project_root: &Path) -> Result<Vec<(String, Harness)>> {
    shadowed_project_skills_in(project_root, &GlobalStore::resolve()?)
}

pub fn shadowed_project_skills_in(
    project_root: &Path,
    store: &GlobalStore,
) -> Result<Vec<(String, Harness)>> {
    let dir = project_dir(project_root, BundledKind::Skill);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let harnesses = harnesses_in_use(project_root);
    let mut names: Vec<String> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        for h in &harnesses {
            if h.project_skill_shadowed_by_global(&store.home, &name) {
                out.push((name.clone(), *h));
            }
        }
    }
    Ok(out)
}

/// Project-tier files with a bundled name whose content equals the current
/// bundle (as `Kind 'name'`): an override that changes nothing.
pub fn redundant_overrides(project_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for kind in [
        BundledKind::Skill,
        BundledKind::Agent,
        BundledKind::Workflow,
    ] {
        let dir = project_dir(project_root, kind);
        for item in items_of_kind(kind) {
            if is_installed(&dir, item) && is_up_to_date(&dir, item) {
                out.push(format!("{} '{}'", kind.label(), item.name));
            }
        }
    }
    out
}

// --- Migration: project bundled copies → global tier ---

fn migration_marker(project_root: &Path) -> PathBuf {
    paths::migrations_dir(project_root).join(MIGRATION_MARKER)
}

/// Whether this project's bundled copies have been migrated to the global
/// tier. Until then, bundled-named project files are stale pm-owned copies;
/// after, they are the user's customs.
pub fn is_migrated(project_root: &Path) -> bool {
    migration_marker(project_root).exists()
}

pub fn write_migration_marker(project_root: &Path) -> Result<()> {
    write_atomic(&migration_marker(project_root), b"")
}

/// The project-tier paths a migration removes: every bundled skill/agent
/// file in main's and each feature worktree's canonical store and harness
/// projections, the baseline (canonical and legacy), and the bundled
/// workflow directories. Only what exists on disk.
pub fn stale_bundled_copies(project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for base in worktrees_on_disk(project_root)? {
        let mut stores = vec![PathBuf::from(CANONICAL_DIR)];
        stores.extend(
            harnesses_in_use(project_root)
                .into_iter()
                .map(|h| PathBuf::from(h.config_dir())),
        );
        for store in stores {
            for kind in [BundledKind::Skill, BundledKind::Agent] {
                let dir = base.join(&store).join(kind.store_subdir().unwrap());
                for item in items_of_kind(kind) {
                    for (rel, _) in item.files {
                        let path = dir.join(rel);
                        if path.exists() {
                            out.push(path);
                        }
                    }
                }
            }
        }
    }
    for path in [
        project_dir(project_root, BundledKind::Baseline).join(BASELINE_FILE),
        legacy_baseline_path(project_root),
    ] {
        if path.exists() {
            out.push(path);
        }
    }
    let workflows = paths::workflows_dir(project_root);
    for item in items_of_kind(BundledKind::Workflow) {
        let dir = workflows.join(item.name);
        if dir.is_dir() {
            out.push(dir);
        }
    }
    Ok(out)
}

/// Main plus every feature worktree that exists on disk.
fn worktrees_on_disk(project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![paths::main_worktree(project_root)];
    let features_dir = paths::features_dir(project_root);
    if features_dir.is_dir() {
        for (name, _) in crate::state::feature::FeatureState::list(&features_dir)? {
            let wt = project_root.join(&name);
            if wt.is_dir() {
                out.push(wt);
            }
        }
    }
    Ok(out)
}

/// Remove the project's bundled copies (see [`stale_bundled_copies`]) and
/// write the migration marker. Returns the removed paths relative to the
/// project root; with `dry_run` nothing is written and the same list says
/// what would go.
pub fn migrate_project_to_global(project_root: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let paths = stale_bundled_copies(project_root)?;
    if dry_run {
        return Ok(paths.iter().map(|p| relative(project_root, p)).collect());
    }
    for path in &paths {
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
            // Stop at the store root (`.agents/` or `.claude/`): only the
            // bundled subtree is pm's.
            let stop = store_root(project_root, path);
            prune_empty_parents(path, &stop);
        }
    }
    for base in worktrees_on_disk(project_root)? {
        let canonical = base.join(CANONICAL_DIR);
        if canonical.read_dir().is_ok_and(|mut d| d.next().is_none()) {
            let _ = fs::remove_dir(&canonical);
        }
    }
    write_migration_marker(project_root)?;
    Ok(paths.iter().map(|p| relative(project_root, p)).collect())
}

/// `<worktree>/<store>` for a path inside a worktree's canonical store or
/// harness dir — the ancestor two levels below the project root.
fn store_root(project_root: &Path, path: &Path) -> PathBuf {
    let rel = path.strip_prefix(project_root).unwrap_or(path);
    let mut root = project_root.to_path_buf();
    for c in rel.components().take(2) {
        root.push(c);
    }
    root
}

fn relative(project_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(project_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

// --- Public API: Skills ---

pub fn skills_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_kind(BundledKind::Skill, project_root)
}

pub fn skills_install(name: Option<&str>) -> Result<Vec<String>> {
    install_kind_global(BundledKind::Skill, name)
}

pub fn skills_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_global(BundledKind::Skill, name)
}

fn install_kind_global(kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let store = GlobalStore::resolve()?;
    let mut messages = install_in(&store.dir(kind), kind, name)?;
    messages.extend(project_global(&store, false)?);
    Ok(messages)
}

fn uninstall_global(kind: BundledKind, name: Option<&str>) -> Result<Vec<String>> {
    let store = GlobalStore::resolve()?;
    let messages = uninstall_in(&store.dir(kind), kind, name)?;
    uninstall_projected_global(&store, kind, name)?;
    Ok(messages)
}

/// Copy main's custom skills — the canonical store and each harness's
/// projection — into the feature worktree's matching directories. Returns
/// the directories copied (relative to a worktree); empty when main has none.
pub fn skills_pull(project_root: &Path, feature_name: &str) -> Result<Vec<PathBuf>> {
    super::claude_settings::require_feature(project_root, feature_name)?;

    let main = paths::main_worktree(project_root);
    let feature = project_root.join(feature_name);
    let mut rels = vec![PathBuf::from(CANONICAL_DIR).join("skills")];
    for h in harnesses_in_use(project_root) {
        rels.push(PathBuf::from(h.config_dir()).join("skills"));
    }
    let present: Vec<PathBuf> = rels.into_iter().filter(|r| main.join(r).is_dir()).collect();
    for rel in &present {
        copy_dir_recursive(&main.join(rel), &feature.join(rel))?;
    }
    Ok(present)
}

// --- Public API: Agents ---

pub fn agents_list(project_root: Option<&Path>) -> Result<Vec<String>> {
    list_kind(BundledKind::Agent, project_root)
}

pub fn agents_install(name: Option<&str>) -> Result<Vec<String>> {
    install_kind_global(BundledKind::Agent, name)
}

pub fn agents_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    uninstall_global(BundledKind::Agent, name)
}

// --- Public API: Baseline ---

/// Absolute path of the shared baseline to apply for this project: the
/// global `~/.agents/pm-baseline.md`. A project spawning on a new binary
/// before its `pm upgrade` ran may only have the pre-migration project copy
/// (`main/.agents/`, or the older `main/.claude/`), so those are returned
/// when the global one is absent; the caller checks existence either way.
pub fn baseline_path(project_root: &Path) -> PathBuf {
    baseline_path_in(project_root, GlobalStore::resolve().ok().as_ref())
}

pub fn baseline_path_in(project_root: &Path, store: Option<&GlobalStore>) -> PathBuf {
    let global = store.map(GlobalStore::baseline_path);
    if let Some(g) = &global
        && g.exists()
    {
        return g.clone();
    }
    let canonical = project_dir(project_root, BundledKind::Baseline).join(BASELINE_FILE);
    if canonical.exists() {
        return canonical;
    }
    let legacy = legacy_baseline_path(project_root);
    if legacy.exists() {
        return legacy;
    }
    global.unwrap_or(canonical)
}

/// Where releases before the canonical store installed the baseline.
fn legacy_baseline_path(project_root: &Path) -> PathBuf {
    paths::main_worktree(project_root)
        .join(Harness::ClaudeCode.config_dir())
        .join(BASELINE_FILE)
}

// --- Public API: Workflows ---

/// Whether `name` is one of the bundled workflows — pm-owned in the global
/// tier, where `pm upgrade` rewrites them. A same-named project workflow is
/// a user override.
pub fn is_bundled_workflow(name: &str) -> bool {
    items_of_kind(BundledKind::Workflow).any(|i| i.name == name)
}

/// (Re)install bundled workflows into the global tier, overwriting any
/// on-disk content — the way to revert a hand-edited global copy.
pub fn workflows_install(name: Option<&str>) -> Result<Vec<String>> {
    let store = GlobalStore::resolve()?;
    install_in(&store.workflows_dir(), BundledKind::Workflow, name)
}

pub fn workflows_uninstall(name: Option<&str>) -> Result<Vec<String>> {
    let store = GlobalStore::resolve()?;
    uninstall_in(&store.workflows_dir(), BundledKind::Workflow, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: BundledKind, name: &str) -> &'static BundledItem {
        find_item(kind, name).unwrap()
    }

    // --- Install / uninstall primitives ---

    #[test]
    fn install_by_name_and_check_status() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "pm")),
            "not installed"
        );

        let messages = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(messages, vec!["Installed Skill 'pm'".to_string()]);
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "pm")),
            "installed"
        );
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "messaging")),
            "not installed"
        );

        let second = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert!(second[0].contains("already up to date"));
        assert!(
            install_in_dry_run(&dir, BundledKind::Skill, Some("pm"))
                .unwrap()
                .is_empty()
        );

        fs::write(dir.join("pm/SKILL.md"), "old content").unwrap();
        assert_eq!(
            status_label(&dir, item(BundledKind::Skill, "pm")),
            "outdated"
        );
        assert_eq!(
            install_in_dry_run(&dir, BundledKind::Skill, Some("pm")).unwrap(),
            vec!["Would update Skill 'pm'".to_string()]
        );
        let third = install_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(third, vec!["Rewrote Skill 'pm'".to_string()]);
        assert!(is_up_to_date(&dir, item(BundledKind::Skill, "pm")));
    }

    #[test]
    fn install_all_of_a_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        for kind in BundledKind::ALL {
            let count = items_of_kind(kind).count();
            let messages = install_in(&dir, kind, None).unwrap();
            assert_eq!(messages.len(), count);
            for item in items_of_kind(kind) {
                assert!(is_installed(&dir, item), "{}", item.name);
            }
        }
        // Workflows install both files.
        let wf = dir.join("implement-and-review");
        assert!(wf.join("config.toml").exists() && wf.join("workflow.md").exists());
    }

    #[test]
    fn install_unknown_name_fails_per_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        for (kind, check) in [
            (
                BundledKind::Skill,
                (|e: &PmError| matches!(e, PmError::SkillNotFound(_))) as fn(&PmError) -> bool,
            ),
            (BundledKind::Agent, |e| {
                matches!(e, PmError::AgentNotFound(_))
            }),
            (BundledKind::Workflow, |e| {
                matches!(e, PmError::WorkflowNotFound(_))
            }),
        ] {
            let err = install_in(&dir, kind, Some("nonexistent")).unwrap_err();
            assert!(check(&err), "{err}");
        }
    }

    #[test]
    fn uninstall_removes_files_and_prunes_empty_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        install_in(&dir, BundledKind::Skill, None).unwrap();
        let user_skill = dir.join("mine/SKILL.md");
        fs::create_dir_all(user_skill.parent().unwrap()).unwrap();
        fs::write(&user_skill, "mine").unwrap();

        let messages = uninstall_in(&dir, BundledKind::Skill, Some("pm")).unwrap();
        assert_eq!(messages, vec!["Uninstalled Skill 'pm'".to_string()]);
        assert!(!dir.join("pm").exists());
        assert!(dir.join("messaging/SKILL.md").exists());

        let messages = uninstall_in(&dir, BundledKind::Skill, None).unwrap();
        assert!(messages.iter().any(|m| m.contains("'pm' is not installed")));
        for item in items_of_kind(BundledKind::Skill) {
            assert!(!is_installed(&dir, item));
        }
        assert_eq!(fs::read_to_string(&user_skill).unwrap(), "mine");
        assert!(dir.exists());
    }

    // --- Global tier ---

    #[test]
    fn install_global_writes_every_kind_and_projects_into_harness_dirs() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        assert_eq!(global_store_missing_in(&store).len(), BUNDLED_ITEMS.len());

        let lines = install_global_in(&store).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l == "Installed Agent 'reviewer' (global)"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "Installed Workflow 'solo' (global)"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Projected") && l.contains("for claude-code")),
            "{lines:?}"
        );

        let h = home.path();
        assert!(h.join(".agents/skills/pm/SKILL.md").exists());
        assert!(h.join(".agents/agents/reviewer.md").exists());
        assert!(h.join(".agents/pm-baseline.md").exists());
        assert!(h.join(".config/pm/workflows/solo/config.toml").exists());
        assert_eq!(store.workflows_dir(), h.join(".config/pm/workflows"));
        for harness in Harness::SUPPORTED {
            let dir = harness.global_config_dir(h).unwrap();
            assert!(dir.join("agents/reviewer.md").exists(), "{harness}");
            assert!(dir.join("skills/pm/SKILL.md").exists(), "{harness}");
            // The baseline is passed by absolute path, never projected.
            assert!(!dir.join("pm-baseline.md").exists(), "{harness}");
        }
        assert!(global_store_missing_in(&store).is_empty());
        assert!(
            unprojected_global_definitions_in(&store)
                .unwrap()
                .is_empty()
        );

        // Idempotent: nothing left to do.
        assert!(install_global_dry_run_in(&store).unwrap().is_empty());
        assert!(install_global_in(&store).unwrap().is_empty());

        // A hand-edited global bundled file is rewritten on the next install.
        fs::write(h.join(".agents/agents/reviewer.md"), "edited").unwrap();
        let dry = install_global_dry_run_in(&store).unwrap();
        assert_eq!(dry[0], "Would update Agent 'reviewer' (global)");
        assert!(dry[1].starts_with("Would project"), "{dry:?}");
        assert_eq!(
            fs::read_to_string(h.join(".agents/agents/reviewer.md")).unwrap(),
            "edited"
        );
        install_global_in(&store).unwrap();
        assert_eq!(
            fs::read_to_string(h.join(".agents/agents/reviewer.md")).unwrap(),
            item(BundledKind::Agent, "reviewer").files[0].1
        );
    }

    #[test]
    fn global_uninstall_removes_projections_and_reports_unprojected_customs() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        install_global_in(&store).unwrap();

        uninstall_in(
            &store.dir(BundledKind::Agent),
            BundledKind::Agent,
            Some("reviewer"),
        )
        .unwrap();
        uninstall_projected_global(&store, BundledKind::Agent, Some("reviewer")).unwrap();
        for h in Harness::SUPPORTED {
            assert!(
                !h.global_config_dir(home.path())
                    .unwrap()
                    .join("agents/reviewer.md")
                    .exists()
            );
        }
        assert_eq!(
            global_store_missing_in(&store),
            vec!["Agent 'reviewer'".to_string()]
        );

        // A user's global custom def is flagged until projected.
        fs::write(home.path().join(".agents/agents/planner.md"), "# planner").unwrap();
        assert_eq!(
            unprojected_global_definitions_in(&store).unwrap(),
            vec![("planner".to_string(), Harness::ClaudeCode)]
        );
        install_global_in(&store).unwrap();
        assert!(
            unprojected_global_definitions_in(&store)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(home.path().join(".claude/agents/planner.md")).unwrap(),
            "# planner"
        );
    }

    #[test]
    fn list_kind_reports_global_status_and_project_override() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        install_global().unwrap();
        let lines = agents_list(Some(project_root)).unwrap();
        assert!(
            lines.contains(&"  reviewer — installed".to_string()),
            "{lines:?}"
        );

        let custom = paths::main_worktree(project_root).join(".agents/agents/reviewer.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "mine").unwrap();
        let lines = agents_list(Some(project_root)).unwrap();
        assert!(
            lines.contains(&"  reviewer — installed (overridden by project custom)".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"  implementer — installed".to_string()),
            "{lines:?}"
        );
        assert!(
            agents_list(None)
                .unwrap()
                .iter()
                .all(|l| !l.contains("overridden"))
        );
    }

    // --- Project projection ---

    #[test]
    fn project_assets_projects_customs_and_reports_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        let claude_agents = main.join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(claude_agents.join("mine.md"), "user's own").unwrap();
        fs::write(claude_agents.join("shared.md"), "harness copy").unwrap();
        fs::write(claude_agents.join("reviewer.md"), "stale").unwrap();

        // Nothing canonical yet: dry-run and real projection are both no-ops.
        assert!(project_assets(project_root, true).unwrap().is_empty());
        assert!(project_assets(project_root, false).unwrap().is_empty());

        let canonical = main.join(".agents/agents");
        fs::create_dir_all(&canonical).unwrap();
        fs::write(canonical.join("shared.md"), "canonical").unwrap();
        // A project override of a bundled name is a custom like any other.
        fs::write(canonical.join("reviewer.md"), "my reviewer").unwrap();

        let dry = project_assets(project_root, true).unwrap();
        assert_eq!(dry.len(), 1, "{dry:?}");
        assert!(dry[0].starts_with("Would project"), "{}", dry[0]);
        assert_eq!(
            fs::read_to_string(claude_agents.join("shared.md")).unwrap(),
            "harness copy"
        );

        let lines = project_assets(project_root, false).unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("for claude-code"), "{}", lines[0]);
        assert!(
            lines[0].contains("replaced: agents/shared.md") && !lines[0].contains("reviewer.md"),
            "{}",
            lines[0]
        );
        assert_eq!(
            fs::read_to_string(claude_agents.join("reviewer.md")).unwrap(),
            "my reviewer"
        );
        assert_eq!(
            fs::read_to_string(claude_agents.join("mine.md")).unwrap(),
            "user's own"
        );
        assert_eq!(
            fs::read_to_string(claude_agents.join("shared.md")).unwrap(),
            "canonical"
        );
        assert!(project_assets(project_root, true).unwrap().is_empty());
    }

    // --- skills pull ---

    fn project_with_feature(project_root: &Path) {
        let features_dir = paths::features_dir(project_root);
        fs::create_dir_all(&features_dir).unwrap();
        fs::write(
            features_dir.join("my-feat.toml"),
            "status = \"wip\"\nbranch = \"my-feat\"\nworktree = \"my-feat\"\nbase = \"main\"\n\
             pr = \"\"\ncontext = \"\"\ncreated = \"2026-01-01T00:00:00Z\"\n\
             last_active = \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();
        fs::create_dir_all(project_root.join("my-feat")).unwrap();
    }

    #[test]
    fn pull_copies_custom_skills_from_both_stores_and_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        project_with_feature(project_root);
        let main = paths::main_worktree(project_root);
        for store in [".agents", ".claude"] {
            let dir = main.join(store).join("skills/foo");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("SKILL.md"), "updated content").unwrap();
        }
        let feature_skill = project_root.join("my-feat/.claude/skills/foo/SKILL.md");
        fs::create_dir_all(feature_skill.parent().unwrap()).unwrap();
        fs::write(&feature_skill, "old content").unwrap();

        let copied = skills_pull(project_root, "my-feat").unwrap();
        assert_eq!(copied.len(), 2, "{copied:?}");
        assert_eq!(
            fs::read_to_string(&feature_skill).unwrap(),
            "updated content"
        );
        assert_eq!(
            fs::read_to_string(project_root.join("my-feat/.agents/skills/foo/SKILL.md")).unwrap(),
            "updated content"
        );
    }

    #[test]
    fn pull_is_a_noop_without_customs_and_errors_on_unknown_feature() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        project_with_feature(project_root);
        assert!(skills_pull(project_root, "my-feat").unwrap().is_empty());
        assert!(!project_root.join("my-feat/.agents").exists());

        let err = skills_pull(project_root, "nonexistent").unwrap_err();
        assert!(matches!(err, PmError::FeatureNotFound(_)));
    }

    // --- Baseline ---

    #[test]
    fn baseline_path_prefers_global_then_project_then_legacy() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        let canonical = main.join(".agents/pm-baseline.md");
        let legacy = main.join(".claude/pm-baseline.md");
        let global = store.baseline_path();

        // Nothing installed: the global path, for callers to test.
        assert_eq!(baseline_path_in(project_root, Some(&store)), global);
        assert!(!global.exists());

        for (path, content) in [
            (&legacy, "legacy"),
            (&canonical, "project"),
            (&global, "global"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
            assert_eq!(&baseline_path_in(project_root, Some(&store)), path);
        }
        // No resolvable home: the project chain still applies.
        assert_eq!(baseline_path_in(project_root, None), canonical);
    }

    // --- Migration ---

    /// A pre-migration project: bundled copies in main's canonical store and
    /// harness dir, a seeded feature, both baselines, bundled and custom
    /// workflows, and customs beside the bundled files.
    fn pre_migration_project(project_root: &Path) {
        project_with_feature(project_root);
        for base in [
            paths::main_worktree(project_root),
            project_root.join("my-feat"),
        ] {
            for store in [".agents", ".claude"] {
                let dir = base.join(store);
                install_in(&dir.join("agents"), BundledKind::Agent, None).unwrap();
                install_in(&dir.join("skills"), BundledKind::Skill, None).unwrap();
                fs::write(dir.join("agents/custom.md"), "custom def").unwrap();
                fs::create_dir_all(dir.join("skills/mine")).unwrap();
                fs::write(dir.join("skills/mine/SKILL.md"), "custom skill").unwrap();
            }
        }
        let main = paths::main_worktree(project_root);
        fs::write(main.join(".agents/pm-baseline.md"), "baseline").unwrap();
        fs::write(main.join(".claude/pm-baseline.md"), "legacy baseline").unwrap();
        fs::write(main.join(".claude/settings.json"), "{}").unwrap();
        let workflows = paths::workflows_dir(project_root);
        install_in(&workflows, BundledKind::Workflow, None).unwrap();
        fs::write(
            workflows.join("solo/config.toml"),
            "description = \"old\"\nagents = [\"claude\"]\n",
        )
        .unwrap();
        fs::create_dir_all(workflows.join("my-flow")).unwrap();
        fs::write(
            workflows.join("my-flow/config.toml"),
            "description = \"mine\"\n",
        )
        .unwrap();
    }

    #[test]
    fn migration_removes_bundled_copies_everywhere_and_keeps_customs() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        pre_migration_project(project_root);
        let main = paths::main_worktree(project_root);
        let feat = project_root.join("my-feat");
        assert!(!is_migrated(project_root));

        let dry = migrate_project_to_global(project_root, true).unwrap();
        assert!(
            dry.contains(&PathBuf::from("main/.claude/agents/reviewer.md")),
            "{dry:?}"
        );
        assert!(
            dry.contains(&PathBuf::from("my-feat/.agents/skills/pm/SKILL.md")),
            "{dry:?}"
        );
        assert!(
            dry.contains(&PathBuf::from("main/.claude/pm-baseline.md")),
            "{dry:?}"
        );
        assert!(
            dry.contains(&PathBuf::from(".pm/workflows/solo")),
            "{dry:?}"
        );
        assert!(
            !dry.iter().any(|p| p.to_string_lossy().contains("custom")
                || p.to_string_lossy().contains("mine")
                || p.to_string_lossy().contains("my-flow")),
            "{dry:?}"
        );
        assert!(main.join(".claude/agents/reviewer.md").exists());
        assert!(!is_migrated(project_root));

        let removed = migrate_project_to_global(project_root, false).unwrap();
        assert_eq!(removed, dry);
        assert!(is_migrated(project_root));

        for base in [&main, &feat] {
            for store in [".agents", ".claude"] {
                let dir = base.join(store);
                for item in items_of_kind(BundledKind::Agent) {
                    assert!(
                        !dir.join("agents").join(item.files[0].0).exists(),
                        "{}",
                        dir.display()
                    );
                }
                assert!(!dir.join("skills/pm").exists(), "{}", dir.display());
                assert_eq!(
                    fs::read_to_string(dir.join("agents/custom.md")).unwrap(),
                    "custom def"
                );
                assert_eq!(
                    fs::read_to_string(dir.join("skills/mine/SKILL.md")).unwrap(),
                    "custom skill"
                );
            }
        }
        assert!(!main.join(".agents/pm-baseline.md").exists());
        assert!(!main.join(".claude/pm-baseline.md").exists());
        assert_eq!(
            fs::read_to_string(main.join(".claude/settings.json")).unwrap(),
            "{}"
        );
        let workflows = paths::workflows_dir(project_root);
        for item in items_of_kind(BundledKind::Workflow) {
            assert!(!workflows.join(item.name).exists(), "{}", item.name);
        }
        assert!(workflows.join("my-flow/config.toml").exists());

        // Nothing bundled-named remains, so a second pass is empty …
        assert!(
            migrate_project_to_global(project_root, true)
                .unwrap()
                .is_empty()
        );
        // … and after the marker, a bundled-named file is a custom override.
        fs::write(main.join(".agents/agents/reviewer.md"), "my reviewer").unwrap();
        assert!(stale_bundled_copies(project_root).unwrap().len() == 1);
        assert!(is_migrated(project_root));
    }

    #[test]
    fn migration_prunes_empty_stores_but_never_the_harness_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let main = paths::main_worktree(project_root);
        fs::create_dir_all(&main).unwrap();
        install_in(&main.join(".agents/agents"), BundledKind::Agent, None).unwrap();
        install_in(&main.join(".agents/skills"), BundledKind::Skill, None).unwrap();
        install_in(&main.join(".claude/agents"), BundledKind::Agent, None).unwrap();
        fs::write(main.join(".claude/settings.json"), "{}").unwrap();

        migrate_project_to_global(project_root, false).unwrap();
        assert!(!main.join(".agents").exists());
        assert!(!main.join(".claude/agents").exists());
        assert!(main.join(".claude/settings.json").exists());
    }

    #[test]
    fn restored_state_without_stores_still_migrates_bundled_workflows() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        fs::create_dir_all(paths::main_worktree(project_root)).unwrap();
        let solo = paths::workflows_dir(project_root).join("solo");
        fs::create_dir_all(&solo).unwrap();
        fs::write(
            solo.join("config.toml"),
            "description = \"old\"\nagents = [\"claude\"]\n",
        )
        .unwrap();

        let removed = migrate_project_to_global(project_root, false).unwrap();
        assert_eq!(removed, vec![PathBuf::from(".pm/workflows/solo")]);
        assert!(!solo.exists());
        assert!(is_migrated(project_root));
    }

    #[test]
    fn redundant_override_is_a_bundled_named_file_with_bundled_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let agents = paths::main_worktree(project_root).join(".agents/agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("reviewer.md"), "my reviewer").unwrap();
        assert!(redundant_overrides(project_root).is_empty());

        install_in(&agents, BundledKind::Agent, Some("reviewer")).unwrap();
        install_in(
            &paths::workflows_dir(project_root),
            BundledKind::Workflow,
            Some("solo"),
        )
        .unwrap();
        assert_eq!(
            redundant_overrides(project_root),
            vec![
                "Agent 'reviewer'".to_string(),
                "Workflow 'solo'".to_string()
            ]
        );
    }

    #[test]
    fn project_skill_is_shadowed_when_the_personal_dir_has_its_name() {
        let home = tempfile::tempdir().unwrap();
        let store = GlobalStore::at(home.path());
        install_global_in(&store).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path();
        let skills = paths::main_worktree(project_root).join(".agents/skills");
        for name in ["pm", "mine"] {
            fs::create_dir_all(skills.join(name)).unwrap();
            fs::write(skills.join(name).join("SKILL.md"), "custom").unwrap();
        }
        assert_eq!(
            shadowed_project_skills_in(project_root, &store).unwrap(),
            vec![("pm".to_string(), Harness::ClaudeCode)]
        );
    }

    #[test]
    fn bundled_workflow_names_are_reserved() {
        assert!(is_bundled_workflow("solo"));
        assert!(!is_bundled_workflow("my-solo"));
    }
}
