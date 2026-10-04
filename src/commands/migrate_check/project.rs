//! One project's findings: its repo and feature branches against origin,
//! uncommitted work in its worktrees, its `.pm/` state repo, the registry
//! fields `pm restore` needs, and agents still running.

use crate::commands::running_agents::{self, Windows};
use crate::commands::skills;
use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectConfig, ProjectEntry, harness_config_in};
use crate::{git, path_utils};

use super::repo::{self, Remote};
use super::worktree::{settings_findings, worktree_findings};
use super::{Finding, Harnesses, shell_path};

/// One project's findings and the harnesses its agents run on, each with
/// the `[harness.*]` settings in effect there.
pub(super) fn findings(
    name: &str,
    entry: &ProjectEntry,
    windows: Option<&Windows>,
    global_harness: &HarnessConfig,
) -> Result<(Vec<Finding>, Harnesses)> {
    let root = entry.root_path();
    let main = paths::main_worktree(&root);
    let pm_dir = paths::pm_dir(&root);
    let mut out = Vec::new();

    if !path_utils::is_portable(&entry.root) {
        out.push(Finding::blocker(
            format!(
                "registry root \"{}\" is relative, so no host can resolve it",
                entry.root
            ),
            format!("pm delete --project {name}, then `pm register` it from its directory"),
        ));
        return Ok((out, Vec::new()));
    }
    if !main.is_dir() {
        out.push(Finding::blocker(
            format!(
                "{} does not exist here, so nothing can be checked",
                shell_path(&main)
            ),
            "run the check on the machine that has it, or leave it out with `--project`"
                .to_string(),
        ));
        return Ok((out, Vec::new()));
    }
    let main_arg = shell_path(&main);
    let in_main = |cmd: &str| format!("cd {main_arg} && {cmd}");
    let backfill = "pm state backfill, then pm state push --global";

    if root.file_name().and_then(|n| n.to_str()) != Some(name) {
        out.push(Finding::note(format!(
            "the registry names it '{name}' but its directory is '{}': `pm restore` registers \
             a clone under the directory name",
            root.file_name().unwrap_or_default().to_string_lossy()
        )));
    }

    // The repo.
    let origin = git::remote_url(&main, "origin")?;
    match (&entry.repo_url, &origin) {
        (None, Some(_)) => out.push(Finding::blocker(
            "the registry has no repo_url, so `pm restore` can't clone it".to_string(),
            backfill.to_string(),
        )),
        (None, None) => out.push(Finding::blocker(
            "the repo has no origin and the registry no repo_url: nothing to clone from"
                .to_string(),
            format!(
                "git -C {main_arg} remote add origin <url> && git -C {main_arg} push -u origin \
                 {}, then {backfill}",
                entry.main_branch
            ),
        )),
        (Some(url), Some(actual)) if url != actual => out.push(Finding::note(format!(
            "the registry clones {url} but origin is {actual}"
        ))),
        _ => {}
    }
    let remote = Remote::of(&main)?;
    let push_branch = |branch: &str| format!("git -C {main_arg} push -u origin {branch}");
    let branch = &entry.main_branch;
    if let Some(remote) = &remote {
        out.extend(repo::branch_finding(
            &main,
            remote,
            branch,
            &format!("main ({branch})"),
            &push_branch(branch),
            None,
        )?);
    }
    out.extend(worktree_findings(
        &main,
        "main worktree",
        &push_branch(branch),
        false,
    )?);

    // Features.
    let features = FeatureState::list(&paths::features_dir(&root)).unwrap_or_default();
    let branches: Vec<&str> = features.iter().map(|(_, f)| f.branch.as_str()).collect();
    let mut bases: Vec<&str> = Vec::new();
    for (feature, state) in &features {
        if state.status == FeatureStatus::Merged {
            continue;
        }
        let label = format!("feature '{feature}'");
        if let Some(remote) = &remote {
            out.extend(repo::branch_finding(
                &main,
                remote,
                &state.branch,
                &label,
                &push_branch(&state.branch),
                None,
            )?);
        }
        let stacked_off_feature =
            state.base == entry.main_branch || branches.contains(&state.base.as_str());
        if !stacked_off_feature && !bases.contains(&state.base.as_str()) {
            bases.push(&state.base);
            if let Some(remote) = &remote {
                out.extend(repo::branch_finding(
                    &main,
                    remote,
                    &state.base,
                    &format!("base {} (features stack on it)", state.base),
                    &push_branch(&state.base),
                    None,
                )?);
            }
            out.push(Finding::note(format!(
                "features stack on {}, which is no feature's branch: on the new host it is \
                 reached as origin/{}, with no local branch",
                state.base, state.base
            )));
        }
        if !state.status.is_active() {
            out.push(Finding::note(format!(
                "{label} is {}: `pm restore` recreates no worktree for it",
                state.status
            )));
        }
        let worktree = root.join(&state.worktree);
        if worktree.is_dir() {
            out.extend(worktree_findings(
                &worktree,
                &label,
                &push_branch(&state.branch),
                true,
            )?);
        }
    }

    // `.pm/` state.
    if !git::is_git_repo(&pm_dir) {
        out.push(Finding::blocker(
            ".pm/ state is not a git repo".to_string(),
            in_main("pm state init --remote <new empty repo url>"),
        ));
    } else {
        let push = in_main("pm state push");
        let state_remote = Remote::of(&pm_dir)?;
        match (&entry.state_remote, &state_remote) {
            (_, None) => out.push(Finding::blocker(
                ".pm/ state has no remote, so its features, agents and messages stay here"
                    .to_string(),
                format!(
                    "{}, then {push}, then {backfill}",
                    in_main("pm state remote <url>")
                ),
            )),
            (None, Some(_)) => out.push(Finding::blocker(
                "the registry has no state_remote, so `pm restore` won't pull .pm/".to_string(),
                backfill.to_string(),
            )),
            _ => {}
        }
        if crate::commands::docs::would_migrate_docs_submodule(&root) {
            out.push(Finding::blocker(
                ".pm/docs/ is a nested git repo, which `pm state push` doesn't carry".to_string(),
                "pm upgrade --all".to_string(),
            ));
        }
        out.extend(repo::state_dirty_finding(&pm_dir, ".pm/ state", &push)?);
        if let Some(state_remote) = &state_remote {
            let branch = git::current_branch(&pm_dir)?;
            out.extend(repo::branch_finding(
                &pm_dir,
                state_remote,
                &branch,
                ".pm/ state",
                &push,
                Some(&in_main("pm state pull")),
            )?);
        }
    }

    // Agents.
    let config = ProjectConfig::load(&pm_dir).ok();
    let project_name = config
        .as_ref()
        .map_or(name.to_string(), |c| c.project.name.clone());
    let mut scopes = vec!["main".to_string()];
    scopes.extend(
        features
            .iter()
            .filter(|(_, f)| f.status.is_active())
            .map(|(n, _)| n.clone()),
    );
    let mut running = Vec::new();
    let mut used = Vec::new();
    let harness_config = harness_config_in(Some(&root), global_harness);
    for harness in skills::harnesses_in_use(&root).unwrap_or_default() {
        used.push(harness);
    }
    for scope in &scopes {
        if let Some(windows) = windows {
            running.extend(
                running_agents::running_in_scope(&root, &project_name, scope, windows)
                    .into_iter()
                    .map(|a| format!("{scope}/{}", a.name)),
            );
        }
        if let Ok(registry) = AgentRegistry::load(&paths::agents_dir(&root), scope) {
            for entry in registry.agents.values() {
                if entry.active && !used.contains(&entry.harness) {
                    used.push(entry.harness);
                }
            }
        }
    }
    let mut worktrees = vec![main.clone()];
    worktrees.extend(
        features
            .iter()
            .filter(|(_, f)| f.status.is_active())
            .map(|(_, f)| root.join(&f.worktree)),
    );
    for harness in &used {
        out.extend(settings_findings(*harness, &worktrees)?);
    }
    if !running.is_empty() {
        out.push(Finding::blocker(
            format!(
                "agents still running ({}): they keep writing .pm/ and their conversations \
                 after the push and export",
                running.join(", ")
            ),
            "pm close --all (agents stay active, so the new host resumes them)".to_string(),
        ));
    }
    let used = used
        .into_iter()
        .map(|h| (h, harness_config.clone()))
        .collect();
    Ok((out, used))
}

#[cfg(test)]
mod tests {
    use super::super::Severity;
    use super::*;
    use crate::testing::TestServer;
    use std::path::Path;
    use tempfile::tempdir;

    fn blockers(name: &str, entry: &ProjectEntry) -> Vec<Finding> {
        findings(name, entry, None, &HarnessConfig::default())
            .unwrap()
            .0
            .into_iter()
            .filter(|f| f.severity == Severity::Blocker)
            .collect()
    }

    fn bare(path: &Path) -> String {
        git::init_bare(path).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn a_project_blocks_until_its_work_and_state_are_on_origin() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (root, name) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&root);
        let pm_dir = paths::pm_dir(&root);
        let repo = bare(&dir.path().join("repo.git"));
        let state = bare(&dir.path().join("state.git"));
        git::add_remote(&main, "origin", &repo).unwrap();
        git::push(&main, "origin", "main").unwrap();
        git::push(&main, "origin", "login").unwrap();
        git::add_remote(&pm_dir, "origin", &state).unwrap();
        git::add_all(&pm_dir).unwrap();
        git::commit_with_message(&pm_dir, "state").unwrap();
        git::push(&pm_dir, "origin", &git::current_branch(&pm_dir).unwrap()).unwrap();
        let mut entry = ProjectEntry {
            root: path_utils::to_portable(&root),
            main_branch: "main".to_string(),
            repo_url: Some(repo),
            state_remote: Some(state),
        };
        assert_eq!(blockers(&name, &entry), []);

        let login = root.join("login");
        std::fs::write(login.join("a.txt"), "a").unwrap();
        git::add_all(&login).unwrap();
        git::commit_with_message(&login, "a").unwrap();
        std::fs::write(main.join("notes.txt"), "secret").unwrap();
        entry.repo_url = None;

        let found = blockers(&name, &entry);
        let about = |what: &str| {
            found
                .iter()
                .find(|f| f.what.contains(what))
                .unwrap_or_else(|| panic!("no finding about {what}: {found:#?}"))
        };
        assert!(
            about("no repo_url")
                .fix
                .as_ref()
                .unwrap()
                .contains("pm state backfill")
        );
        let unpushed = about("feature 'login': 1 commit not pushed");
        assert!(
            unpushed
                .fix
                .as_ref()
                .unwrap()
                .ends_with("push -u origin login")
        );
        // An untracked file may be a secret: the fix never adds everything.
        let untracked = about("notes.txt");
        assert!(!untracked.fix.as_ref().unwrap().contains("add -A"));
        assert_eq!(found.len(), 3, "{found:#?}");
    }

    #[test]
    fn a_diverged_feature_is_rebased_in_its_own_worktree() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (root, name) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&root);
        let repo = bare(&dir.path().join("repo.git"));
        git::add_remote(&main, "origin", &repo).unwrap();
        git::push(&main, "origin", "main").unwrap();
        git::push(&main, "origin", "login").unwrap();

        let other = dir.path().join("other");
        git::clone_repo(&repo, &other).unwrap();
        git::run_git(&other, &["checkout", "-q", "login"]).unwrap();
        std::fs::write(other.join("theirs.txt"), "theirs").unwrap();
        git::add_all(&other).unwrap();
        git::commit_with_message(&other, "theirs").unwrap();
        git::push(&other, "origin", "login").unwrap();
        let login = root.join("login");
        std::fs::write(login.join("ours.txt"), "ours").unwrap();
        git::add_all(&login).unwrap();
        git::commit_with_message(&login, "ours").unwrap();
        git::fetch_remote(&main, "origin").unwrap();

        let entry = ProjectEntry {
            root: path_utils::to_portable(&root),
            main_branch: "main".to_string(),
            repo_url: Some(repo),
            state_remote: None,
        };
        let found = blockers(&name, &entry);
        let diverged = found
            .iter()
            .find(|f| f.what.contains("have diverged"))
            .unwrap_or_else(|| panic!("{found:#?}"));
        let fix = diverged.fix.as_deref().unwrap();
        let login = shell_path(&login.canonicalize().unwrap());
        assert!(
            fix.starts_with(&format!("git -C {login} pull --rebase origin login")),
            "{fix}"
        );
    }
}
