//! The asset tiers: the global store, `[bundled.disable]`, stale or
//! redundant project copies, and definitions a harness has no projection of.

use std::path::Path;

use super::{Fix, FixAction, Issue, IssueKind};
use crate::commands::bundled_disable::Disabled;
use crate::commands::skills;
use crate::error::Result;
use crate::harness::Harness;
use crate::state::paths;
use crate::state::workflow;

/// Main-scope findings about the two asset tiers: what the global tier is
/// missing, pre-migration copies still shadowing it, definitions a harness
/// can't see, and project skills its global namesake shadows.
pub(super) fn asset_issues(
    project_root: &Path,
    projections: &DefinitionProjections,
) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();

    let missing = skills::global_store_missing()?;
    if !missing.is_empty() {
        issues.push(Issue {
            kind: IssueKind::GlobalStoreMissing,
            message: format!(
                "not installed in the global asset store: {} (run `pm upgrade`)",
                missing.join(", ")
            ),
            fix: Fix::Auto(FixAction::InstallGlobalAssets),
        });
    }

    issues.extend(bundled_issues(
        project_root,
        &workflow::global_dir()?,
        paths::home_dir().ok().as_deref(),
        &Disabled::load(),
    )?);
    let still_installed = skills::disabled_still_installed()?;
    if !still_installed.is_empty() {
        issues.push(Issue {
            kind: IssueKind::DisabledStillInstalled,
            message: format!(
                "disabled by `[bundled.disable]` but still installed: {} (run `pm upgrade`)",
                still_installed.join(", ")
            ),
            fix: Fix::Auto(FixAction::InstallGlobalAssets),
        });
    }

    if !skills::is_migrated(project_root) {
        let stale = skills::stale_bundled_copies(project_root)?;
        if !stale.is_empty() {
            issues.push(Issue {
                kind: IssueKind::StaleBundledCopies,
                message: format!(
                    "{} pre-migration bundled copies shadow the global store (run `pm upgrade`)",
                    stale.len()
                ),
                fix: Fix::None,
            });
        }
    } else {
        let redundant = skills::redundant_overrides(project_root);
        if !redundant.is_empty() {
            issues.push(Issue {
                kind: IssueKind::RedundantOverride,
                message: format!(
                    "project overrides identical to the bundled asset (delete to follow the \
                     global store): {}",
                    redundant.join(", ")
                ),
                fix: Fix::None,
            });
        }
    }

    for (name, harness) in unprojected_definitions(projections)? {
        issues.push(Issue {
            kind: IssueKind::AssetNotProjected,
            message: format!(
                "canonical agent '{name}' not projected for {harness} (run `pm upgrade`)"
            ),
            fix: Fix::Skip,
        });
    }

    for (name, harness) in skills::shadowed_project_skills(project_root)? {
        issues.push(Issue {
            kind: IssueKind::SkillShadowedByGlobal,
            message: format!(
                "project skill '{name}' never applies: {harness} resolves the personal skill of \
                 that name over it — rename the custom"
            ),
            fix: Fix::None,
        });
    }

    Ok(issues)
}

/// Findings about the global config's `[bundled.disable]` table: what it disables,
/// names in it pm doesn't bundle, and references to a disabled name that no
/// custom resolves.
fn bundled_issues(
    project_root: &Path,
    global_dir: &Path,
    home: Option<&Path>,
    disabled: &Disabled,
) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    let items = disabled.items();
    if !items.is_empty() {
        issues.push(Issue {
            kind: IssueKind::BundledDisabled,
            message: format!("disabled by `[bundled.disable]`: {}", items.join(", ")),
            fix: Fix::None,
        });
    }
    let unknown = disabled.unknown();
    if !unknown.is_empty() {
        issues.push(Issue {
            kind: IssueKind::BundledUnknown,
            message: format!(
                "`[bundled.disable]` names nothing pm bundles: {}",
                unknown.join(", ")
            ),
            fix: Fix::None,
        });
    }
    for reference in disabled.dangling(project_root, global_dir, home)? {
        issues.push(Issue {
            kind: IssueKind::BundledDisabledDangling,
            message: format!(
                "{reference}, which `[bundled.disable]` lists (add a project custom or re-enable it)"
            ),
            fix: Fix::None,
        });
    }
    Ok(issues)
}

/// Which harnesses need projected definitions, and main's canonical
/// (project-tier) definitions each lacks a projected copy of in main.
pub(super) struct DefinitionProjections {
    harnesses: Vec<Harness>,
    missing_in_main: Vec<(String, Harness)>,
}

impl DefinitionProjections {
    pub(super) fn load(project_root: &Path) -> Result<Self> {
        let harnesses: Vec<Harness> = skills::harnesses_in_use(project_root)?
            .into_iter()
            .filter(|h| h.projects_definitions())
            .collect();
        let main = paths::main_worktree(project_root);
        let missing_in_main = unprojected_project_definitions(&main, &main, &harnesses)?;
        Ok(Self {
            harnesses,
            missing_in_main,
        })
    }
}

/// Agent definitions with no projected copy in a harness's own definition
/// dir, in either tier. Such a definition passes `WorkflowDef::validate`
/// but the harness can't find it at launch.
fn unprojected_definitions(projections: &DefinitionProjections) -> Result<Vec<(String, Harness)>> {
    let mut out = skills::unprojected_global_definitions(&projections.harnesses)?;
    out.extend(projections.missing_in_main.iter().cloned());
    Ok(out)
}

/// Main's canonical (project-tier) definitions with no projected copy in
/// `worktree`'s harness dirs.
fn unprojected_project_definitions(
    main: &Path,
    worktree: &Path,
    harnesses: &[Harness],
) -> Result<Vec<(String, Harness)>> {
    let canonical = main.join(skills::CANONICAL_DIR).join("agents");
    let mut out = Vec::new();
    for file in skills::definition_files(&canonical)? {
        for harness in harnesses {
            let projected = worktree
                .join(harness.config_dir())
                .join("agents")
                .join(&file);
            if !projected.exists() {
                out.push((file.trim_end_matches(".md").to_string(), *harness));
            }
        }
    }
    Ok(out)
}

/// Project-tier definitions a feature worktree lacks a projected copy of.
/// `pm upgrade` projects into main only; the harness, started in the
/// feature, never sees main's copy.
pub(super) fn feature_projection_issues(
    project_root: &Path,
    projections: &DefinitionProjections,
    feature: &str,
    worktree: &Path,
) -> Result<Vec<Issue>> {
    let main = paths::main_worktree(project_root);
    let pull = format!("`pm harness pull {feature}`");
    Ok(
        unprojected_project_definitions(&main, worktree, &projections.harnesses)?
            .into_iter()
            .map(|(name, harness)| {
                let main_lacks = projections
                    .missing_in_main
                    .contains(&(name.clone(), harness));
                let (remedy, fix) = if main_lacks {
                    (format!("run `pm upgrade`, then {pull}"), Fix::None)
                } else {
                    (
                        format!("run {pull}"),
                        Fix::Auto(FixAction::PullFeatureAssets),
                    )
                };
                Issue {
                    kind: IssueKind::AssetNotProjected,
                    message: format!(
                        "canonical agent '{name}' not projected into this worktree for \
                         {harness} ({remedy})"
                    ),
                    fix,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::doctor;
    use crate::commands::doctor::test_support::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn bundled_issues_report_dangling_references_and_unknown_names() {
        let root = tempdir().unwrap();
        let global = tempdir().unwrap();
        let home = tempdir().unwrap();
        let wf = global.path().join("research-implement-review");
        std::fs::create_dir_all(&wf).unwrap();
        std::fs::write(
            wf.join("config.toml"),
            "description = \"d\"\nagents = [\"researcher\", \"reviewer\"]\n",
        )
        .unwrap();
        let disabled = Disabled::from_config(
            toml::from_str("agents = [\"researcher\", \"planner\"]\nworkflows = [\"solo\"]")
                .unwrap(),
        );
        let messages = |d: &Disabled| -> Vec<(IssueKind, String)> {
            bundled_issues(root.path(), global.path(), Some(home.path()), d)
                .unwrap()
                .into_iter()
                .map(|i| (i.kind, i.message))
                .collect()
        };

        let issues = messages(&disabled);
        let of = |kind| {
            issues
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, m)| m.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            of(IssueKind::BundledDisabled),
            vec!["disabled by `[bundled.disable]`: Agent 'researcher', Workflow 'solo'"]
        );
        assert_eq!(
            of(IssueKind::BundledUnknown),
            vec!["`[bundled.disable]` names nothing pm bundles: agents 'planner'"]
        );
        let dangling = of(IssueKind::BundledDisabledDangling);
        assert_eq!(dangling.len(), 2, "{dangling:?}");
        assert!(
            dangling[0]
                .starts_with("workflow 'research-implement-review' needs agent 'researcher'"),
            "{dangling:?}"
        );
        assert!(
            dangling[1].contains("needs workflow 'solo'"),
            "{dangling:?}"
        );

        let agents = paths::main_worktree(root.path()).join(".agents/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("researcher.md"), "# mine").unwrap();
        assert!(
            !messages(&disabled)
                .iter()
                .any(|(_, m)| m.contains("needs agent")),
        );
        assert!(messages(&Disabled::default()).is_empty());
    }

    fn unprojected(project_root: &Path) -> Result<Vec<(String, Harness)>> {
        unprojected_definitions(&DefinitionProjections::load(project_root)?)
    }

    #[test]
    fn unprojected_project_definition_is_flagged_until_projected() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let planner = paths::main_worktree(&project_path).join(".agents/agents/planner.md");
        std::fs::create_dir_all(planner.parent().unwrap()).unwrap();
        std::fs::write(&planner, "# planner").unwrap();

        assert_eq!(
            unprojected(&project_path).unwrap(),
            vec![("planner".to_string(), Harness::ClaudeCode)]
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("canonical agent 'planner' not projected for claude-code")),
            "{lines:?}"
        );

        // `pm upgrade` projects into main only; the feature needs a pull.
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        assert!(unprojected(&project_path).unwrap().is_empty());
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("not projected"))
                .collect::<Vec<_>>(),
            [
                "  login — canonical agent 'planner' not projected into this worktree for \
              claude-code (run `pm harness pull login`)"
            ],
        );

        doctor(&project_path, &projects_dir, true, server.name()).unwrap();
        assert!(
            project_path
                .join("login/.claude/agents/planner.md")
                .is_file()
        );
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("not projected")),
            "{lines:?}"
        );
    }

    #[test]
    fn opencode_definition_without_a_projected_copy_is_flagged() {
        // opencode runs its built-in prompt for a name it cannot find, with
        // no error, so the projected file is the only thing to check.
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _, _) = server.setup_project_no_tmux(dir.path());
        use_opencode(&project_path, "planner", "opencode v2.0.23");
        let planner = paths::main_worktree(&project_path).join(".agents/agents/planner.md");
        std::fs::create_dir_all(planner.parent().unwrap()).unwrap();
        std::fs::write(&planner, "# planner").unwrap();

        assert!(
            unprojected(&project_path)
                .unwrap()
                .contains(&("planner".to_string(), Harness::OpenCode))
        );
        let issues = asset_issues(
            &project_path,
            &DefinitionProjections::load(&project_path).unwrap(),
        )
        .unwrap();
        assert!(
            messages(&issues, IssueKind::AssetNotProjected).contains(
                &"canonical agent 'planner' not projected for opencode (run `pm upgrade`)"
                    .to_string()
            ),
            "{:?}",
            issues.iter().map(Issue::message).collect::<Vec<_>>()
        );

        crate::commands::skills::project_assets(&project_path, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(
                paths::main_worktree(&project_path).join(".opencode/agents/planner.md")
            )
            .unwrap(),
            "# planner"
        );
        assert!(
            !unprojected(&project_path)
                .unwrap()
                .iter()
                .any(|(name, _)| name == "planner")
        );
    }

    #[test]
    fn stale_bundled_copies_flagged_until_migrated_then_redundant_overrides() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let bundled_reviewer = include_str!("../../../assets/agents/reviewer.md");

        // A pre-migration project: bundled copies present, no marker.
        let claude_agents = paths::main_worktree(&project_path).join(".claude/agents");
        std::fs::create_dir_all(&claude_agents).unwrap();
        std::fs::write(claude_agents.join("reviewer.md"), bundled_reviewer).unwrap();
        std::fs::remove_file(paths::migrations_dir(&project_path).join("global-assets")).unwrap();

        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("pre-migration bundled copies shadow")),
            "{lines:?}"
        );

        crate::commands::skills::migrate_project_to_global(&project_path, false).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("pre-migration")),
            "{lines:?}"
        );

        // After the marker, a bundled-named copy is an override — flagged
        // only as redundant when its bytes match the bundle.
        let canonical = paths::main_worktree(&project_path).join(".agents/agents");
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("reviewer.md"), bundled_reviewer).unwrap();
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("identical to the bundled asset")),
            "{lines:?}"
        );

        std::fs::write(canonical.join("reviewer.md"), "my reviewer").unwrap();
        crate::commands::skills::project_assets(&project_path, false).unwrap();
        let lines = doctor(&project_path, &projects_dir, false, server.name())
            .unwrap()
            .lines();
        assert!(
            !lines.iter().any(|l| l.contains("identical to the bundled")),
            "{lines:?}"
        );
    }
}
