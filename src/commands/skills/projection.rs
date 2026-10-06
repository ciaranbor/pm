//! Projecting a canonical store into the layouts harnesses read.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::harness::{self, Harness, ProjectionScope};
use crate::state::paths;
use crate::state::project::{GlobalConfig, ProjectConfig};

use super::bundled::{BundledKind, is_bundled_asset, items_of_kind};
use super::{CANONICAL_DIR, GlobalStore};

/// Where a bundled kind would live in the project tier. After migration
/// only user customs are found here.
pub(super) fn project_dir(project_root: &Path, kind: BundledKind) -> PathBuf {
    let canonical = paths::main_worktree(project_root).join(CANONICAL_DIR);
    match kind {
        BundledKind::Skill | BundledKind::Agent => canonical.join(kind.store_subdir().unwrap()),
        BundledKind::Baseline => canonical,
        BundledKind::Workflow => paths::workflows_dir(project_root),
    }
}

/// The harnesses whose projections this project maintains: the default plus
/// any named in `[agents.harness]`. A missing project config contributes
/// nothing (the default still applies); a malformed one is an error, so the
/// install paths never quietly fall back to Claude Code alone.
pub fn harnesses_in_use(project_root: &Path) -> Result<Vec<Harness>> {
    let project = match ProjectConfig::load(&paths::pm_dir(project_root)) {
        Ok(config) => config.agents,
        Err(crate::error::PmError::NotInProject) => Default::default(),
        Err(e) => return Err(e),
    };
    Ok(harness::harnesses_in_use(
        &project,
        &GlobalConfig::load_or_default().agents,
    ))
}

/// Project the main worktree's canonical store (the project's customs) into
/// every harness in use. Returns one line per harness whose projection
/// changed (`Would project …` in `dry_run`, which writes nothing); in sync
/// yields no lines.
pub fn project_assets(project_root: &Path, dry_run: bool) -> Result<Vec<String>> {
    let main = paths::main_worktree(project_root);
    let canonical = main.join(CANONICAL_DIR);
    let mut lines = Vec::new();
    for h in harnesses_in_use(project_root)? {
        let target = main.join(h.config_dir());
        lines.extend(project_into(&canonical, h, &target, dry_run)?);
    }
    Ok(lines)
}

/// Project the global canonical store into every supported harness's own
/// global dir (`~/.agents` → `~/.claude` for claude-code).
pub(super) fn project_global(store: &GlobalStore, dry_run: bool) -> Result<Vec<String>> {
    project_global_from(&store.canonical(), &store.home, dry_run)
}

pub(super) fn project_global_from(
    canonical: &Path,
    home: &Path,
    dry_run: bool,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for h in Harness::SUPPORTED {
        let Some(target) = h.global_config_dir(home) else {
            continue;
        };
        lines.extend(project_into(canonical, *h, &target, dry_run)?);
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
    let projection = h.project_assets(canonical, target, &ProjectionScope::default(), dry_run)?;
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

/// The global tier's skills and agent definitions pm does not bundle,
/// relative to the canonical store (`skills/<name>`, `agents/<file>`): the
/// user's own, which nothing pm syncs carries to another machine.
pub fn global_customs_in(store: &GlobalStore) -> Vec<String> {
    let mut out = Vec::new();
    for kind in [BundledKind::Skill, BundledKind::Agent] {
        let Ok(entries) = std::fs::read_dir(store.dir(kind)) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .filter(|name| {
                let item = match kind {
                    BundledKind::Agent => name.strip_suffix(".md").unwrap_or(name),
                    _ => name,
                };
                !items_of_kind(kind).any(|i| i.name == item)
            })
            .collect();
        names.sort_unstable();
        let sub = kind.store_subdir().unwrap_or_default();
        out.extend(names.into_iter().map(|name| format!("{sub}/{name}")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn harnesses_in_use_defaults_without_config_and_errors_on_a_malformed_one() {
        let dir = tempfile::tempdir().unwrap();
        let server = crate::testing::TestServer::new();
        let (project, _, _) = server.setup_project_no_tmux(dir.path());
        let config = paths::pm_dir(&project).join("config.toml");

        std::fs::remove_file(&config).unwrap();
        assert_eq!(
            harnesses_in_use(&project).unwrap(),
            vec![Harness::ClaudeCode]
        );

        std::fs::write(&config, "[agents.harness]\n[agents.harness]\n").unwrap();
        assert!(harnesses_in_use(&project).is_err());
    }

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
}
