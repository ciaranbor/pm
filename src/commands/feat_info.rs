use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::commands::feat_sync::sync_one;
use crate::error::Result;
use crate::git::{self, BranchDivergence};
use crate::state::feature::{FeatureState, FeatureStatus, Progress};
use crate::state::paths;
use crate::state::project::ProjectEntry;

/// Human-readable label for a feature status derived from PR state.
fn pr_status_label(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Merged => "merged",
        FeatureStatus::Stale => "closed",
        FeatureStatus::Wip => "draft",
        FeatureStatus::Approved => "approved",
        FeatureStatus::Review => "open",
        FeatureStatus::Initializing => "unknown",
    }
}

/// A feature's details, read from pm state and git without contacting
/// GitHub: `lifecycle` is what the last sync recorded.
#[derive(Debug, Serialize)]
pub struct FeatureInfo {
    pub name: String,
    pub progress: Progress,
    pub lifecycle: FeatureStatus,
    pub branch: String,
    #[serde(skip)]
    pub worktree: String,
    /// The branch the feature stacks on, resolved to the main branch for
    /// features that predate recording it.
    pub base: String,
    /// `None` when git can't compare the branches, e.g. the base is gone.
    pub divergence: Option<BranchDivergence>,
    pub remote: Option<String>,
    pub rebase_in_progress: bool,
    pub pr: Option<String>,
    pub workflow: Option<Workflow>,
    /// The `--context` brief the feature was created with.
    pub context: Option<String>,
    pub has_summary: bool,
    pub created: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct Workflow {
    pub name: String,
    /// `None` when the workflow is missing or unparseable.
    pub description: Option<String>,
}

pub fn info(project_root: &Path, projects_dir: &Path, name: &str) -> Result<FeatureInfo> {
    let state = FeatureState::load(&paths::features_dir(project_root), name)?;
    let main_repo = paths::main_worktree(project_root);
    let main_branch = ProjectEntry::main_branch(project_root, projects_dir)?;
    let base = state.base_branch(&main_branch).to_string();
    let workflow = state.workflow.as_ref().map(|workflow| Workflow {
        name: workflow.clone(),
        description: match crate::commands::workflow::get(project_root, workflow) {
            Ok(Some(def)) => Some(def.description),
            _ => None,
        },
    });
    Ok(FeatureInfo {
        name: name.to_string(),
        progress: state.team.progress,
        lifecycle: state.status,
        divergence: git::branch_divergence(&main_repo, &state.branch, &base).ok(),
        remote: git::remote_tracking_branch(&main_repo, &state.branch).unwrap_or(None),
        rebase_in_progress: git::rebase_in_progress(&project_root.join(&state.worktree))
            .unwrap_or(false),
        pr: Some(state.pr).filter(|pr| !pr.is_empty()),
        workflow,
        context: Some(state.context).filter(|c| !c.is_empty()),
        has_summary: paths::summary_path(project_root, name).exists(),
        branch: state.branch,
        worktree: state.worktree,
        base,
        created: state.created,
        last_active: state.last_active,
    })
}

/// Display full details for a single feature, first syncing its status
/// from its PR. Returns formatted lines for display.
pub fn feat_info(project_root: &Path, projects_dir: &Path, name: &str) -> Result<Vec<String>> {
    let features_dir = paths::features_dir(project_root);
    let mut state = FeatureState::load(&features_dir, name)?;
    let pr_status = (!state.pr.is_empty()).then(|| {
        sync_one(
            &mut state,
            &features_dir,
            name,
            &paths::main_worktree(project_root),
        )
        .map(|()| state.status)
    });
    let info = info(project_root, projects_dir, name)?;

    let mut lines = Vec::new();
    lines.push(format!("name:        {name}"));
    lines.push(format!("status:      {}", info.progress));
    lines.push(format!("lifecycle:   {}", info.lifecycle));
    lines.push(format!(
        "summary:     {}",
        if info.has_summary {
            paths::summary_path(project_root, name)
                .display()
                .to_string()
        } else {
            "none".to_string()
        }
    ));
    lines.push(format!("branch:      {}", info.branch));
    lines.push(format!("worktree:    {}", info.worktree));
    if info.rebase_in_progress {
        lines.push(
            "rebase:      in progress (finish with `git rebase --continue` or `--abort`)"
                .to_string(),
        );
    }
    lines.push(format!(
        "remote:      {}",
        info.remote.as_deref().unwrap_or("None")
    ));
    lines.push(format!("base:        {}", info.base));
    if let Some(pr) = &info.pr {
        lines.push(format!("pr:          #{pr}"));
        lines.push(match pr_status {
            Some(Ok(status)) => format!("pr_status:   {}", pr_status_label(status)),
            _ => "pr_status:   (query failed)".to_string(),
        });
    }
    if let Some(div) = &info.divergence {
        lines.push(format!("divergence:  {div} {}", info.base));
    }
    if let Some(workflow) = &info.workflow {
        lines.push(match &workflow.description {
            Some(description) => format!("workflow:    {} — {description}", workflow.name),
            None => format!("workflow:    {}", workflow.name),
        });
    }
    if let Some(context) = &info.context {
        lines.push(format!("context:     {context}"));
    }
    lines.push(format!(
        "created:     {}",
        info.created.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    lines.push(format!(
        "last_active: {}",
        info.last_active.format("%Y-%m-%d %H:%M:%S UTC")
    ));

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{feat_new, init};
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn feat_info_shows_all_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams {
            project_root: &project_path,
            projects_dir: &projects_dir,
            name: "alpha",
            name_override: None,
            context: Some("fix the widget"),
            base: None,
            workflow: Some("implement-and-review"),
            tmux_server: server.name(),
        })
        .unwrap();

        let lines = feat_info(&project_path, &projects_dir, "alpha").unwrap();
        let output = lines.join("\n");
        assert!(output.contains("name:        alpha"));
        assert!(output.contains("status:      wip"));
        assert!(output.contains("summary:     none"));
        assert!(output.contains("branch:      alpha"));
        assert!(output.contains("worktree:    alpha"));
        assert!(output.contains("remote:      None"));
        // `feat info` shows the workflow name + its description.
        assert!(output.contains("workflow:    implement-and-review — "));
        assert!(output.contains("Implementer drains tasks"));
        assert!(output.contains("context:     fix the widget"));
        assert!(output.contains("created:"));
        assert!(output.contains("last_active:"));

        std::fs::write(
            crate::commands::feat_summary::path(&project_path, "alpha").unwrap(),
            "notes",
        )
        .unwrap();
        let lines = feat_info(&project_path, &projects_dir, "alpha").unwrap();
        let summary = paths::summary_path(&project_path, "alpha");
        assert!(lines.contains(&format!("summary:     {}", summary.display())));
    }

    #[test]
    fn feat_info_shows_a_paused_rebase() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project_path, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project_path);
        let rebase_line = |lines: Vec<String>| lines.into_iter().find(|l| l.starts_with("rebase:"));
        TestServer::add_feature_commit(&project_path, "login");
        assert_eq!(
            rebase_line(feat_info(&project_path, &projects_dir, "login").unwrap()),
            None
        );

        TestServer::pause_rebase(&project_path.join("login"), "main");
        let line = rebase_line(feat_info(&project_path, &projects_dir, "login").unwrap());
        assert!(line.unwrap().contains("in progress"));
    }

    #[test]
    fn feat_info_shows_remote_when_upstream_set() {
        use std::process::Command;

        let dir = tempdir().unwrap();
        let server = TestServer::new();

        // Create a bare "remote" repo
        let bare_path = dir.path().join("remote.git");
        crate::git::init_bare(&bare_path).unwrap();

        // Init project (creates a real git repo at project_path/main)
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let main_repo = paths::main_worktree(&project_path);

        // Add remote to the main git repo
        Command::new("git")
            .args([
                "-C",
                &main_repo.to_string_lossy(),
                "remote",
                "add",
                "origin",
                &bare_path.to_string_lossy(),
            ])
            .output()
            .unwrap();

        // Push main so remote has something
        Command::new("git")
            .args([
                "-C",
                &main_repo.to_string_lossy(),
                "push",
                "-u",
                "origin",
                "main",
            ])
            .output()
            .unwrap();

        // Create a feature (worktree at project_path/tracked)
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "tracked",
            server.name(),
        ))
        .unwrap();

        // Push the feature branch to set up tracking
        let wt_path = project_path.join("tracked");
        Command::new("git")
            .args([
                "-C",
                &wt_path.to_string_lossy(),
                "push",
                "-u",
                "origin",
                "tracked",
            ])
            .output()
            .unwrap();

        let lines = feat_info(&project_path, &projects_dir, "tracked").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("remote:      origin/tracked"),
            "expected remote tracking branch, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();

        let result = feat_info(&project_path, &projects_dir, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn feat_info_omits_empty_optional_fields() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "beta",
            server.name(),
        ))
        .unwrap();

        let features_dir = paths::features_dir(&project_path);
        let mut state = FeatureState::load(&features_dir, "beta").unwrap();
        state.base.clear();
        state.save(&features_dir, "beta").unwrap();

        let lines = feat_info(&project_path, &projects_dir, "beta").unwrap();
        let output = lines.join("\n");
        assert!(output.contains("base:        main"), "{output}");
        assert!(!output.contains("pr:"));
        assert!(!output.contains("context:"));
    }

    #[test]
    fn feat_info_shows_divergence() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "diverge",
            server.name(),
        ))
        .unwrap();

        // Add a commit on the feature branch
        let wt_path = project_path.join("diverge");
        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        git::stage_file(&wt_path, "feat.txt").unwrap();
        git::commit(&wt_path, "feature commit").unwrap();

        let lines = feat_info(&project_path, &projects_dir, "diverge").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  1 commit ahead main"),
            "expected divergence info, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_shows_up_to_date_divergence() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "synced",
            server.name(),
        ))
        .unwrap();

        let lines = feat_info(&project_path, &projects_dir, "synced").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  up to date main"),
            "expected up to date, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_shows_behind_divergence() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "behind",
            server.name(),
        ))
        .unwrap();

        // Add a commit on main (the feature is now behind)
        let main_repo = paths::main_worktree(&project_path);
        std::fs::write(main_repo.join("main.txt"), "content").unwrap();
        git::stage_file(&main_repo, "main.txt").unwrap();
        git::commit(&main_repo, "main commit").unwrap();

        let lines = feat_info(&project_path, &projects_dir, "behind").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  1 commit behind main"),
            "expected behind info, got:\n{output}"
        );
    }

    #[test]
    fn feat_info_shows_ahead_and_behind() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let project_path = dir.path().join(server.scope("myapp"));
        let projects_dir = dir.path().join("registry");
        init::init(&project_path, &projects_dir, None, server.name()).unwrap();
        feat_new::feat_new(&feat_new::FeatNewParams::with_defaults(
            &project_path,
            &projects_dir,
            "both",
            server.name(),
        ))
        .unwrap();

        // Add a commit on the feature branch
        let wt_path = project_path.join("both");
        std::fs::write(wt_path.join("feat.txt"), "content").unwrap();
        git::stage_file(&wt_path, "feat.txt").unwrap();
        git::commit(&wt_path, "feature commit").unwrap();

        // Add a commit on main
        let main_repo = paths::main_worktree(&project_path);
        std::fs::write(main_repo.join("main.txt"), "content").unwrap();
        git::stage_file(&main_repo, "main.txt").unwrap();
        git::commit(&main_repo, "main commit").unwrap();

        let lines = feat_info(&project_path, &projects_dir, "both").unwrap();
        let output = lines.join("\n");
        assert!(
            output.contains("divergence:  1 commit ahead, 1 behind main"),
            "expected ahead and behind, got:\n{output}"
        );
    }

    #[test]
    fn pr_status_label_maps_correctly() {
        use crate::state::feature::FeatureStatus;

        assert_eq!(pr_status_label(FeatureStatus::Review), "open");
        assert_eq!(pr_status_label(FeatureStatus::Wip), "draft");
        assert_eq!(pr_status_label(FeatureStatus::Approved), "approved");
        assert_eq!(pr_status_label(FeatureStatus::Merged), "merged");
        assert_eq!(pr_status_label(FeatureStatus::Stale), "closed");
        assert_eq!(pr_status_label(FeatureStatus::Initializing), "unknown");
    }
}
