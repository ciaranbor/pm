//! One project's findings: its repo and feature branches against origin,
//! uncommitted work in its worktrees, its `.pm/` state repo, the registry
//! fields `pm restore` needs, and agents still running.

use std::path::Path;

use crate::commands::running_agents::Windows;
use crate::commands::skills;
use crate::error::Result;
use crate::harness::Harness;
use crate::state::feature::{FeatureState, FeatureStatus};
use crate::state::paths;
use crate::state::project::{HarnessConfig, ProjectEntry, harness_config_in};
use crate::{git, path_utils};

use super::agents::Agents;
use super::line::Line;
use super::repo::{self, Remote};
use super::worktree::{self, Branch, settings_findings};
use super::{Finding, Harnesses, Step, rel_path, shell_path};

/// The plan steps that put the registry's view of a project right.
const BACKFILL: &[Step] = &[Step::Backfill, Step::GlobalPush];

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
    if let Some(finding) = uncheckable(name, entry, &main) {
        return Ok((vec![finding], Vec::new()));
    }
    let pm_dir = paths::pm_dir(&root);
    let features = FeatureState::list(&paths::features_dir(&root)).unwrap_or_default();
    let agents = Agents::read(&root, name, &pm_dir, &features, windows);
    let mut p = Project {
        name,
        entry,
        root: &root,
        main: &main,
        features: &features,
        out: Vec::new(),
        notes: Vec::new(),
        registry: Line::default(),
    };
    p.directory_note();
    p.out.extend(running(&agents));
    p.branches(&agents)?;
    p.state(&pm_dir)?;

    let mut used = skills::harnesses_in_use(&root).unwrap_or_default();
    for harness in &agents.harnesses {
        if !used.contains(harness) {
            used.push(*harness);
        }
    }
    p.settings(&used)?;
    let Project {
        mut out,
        notes,
        registry,
        ..
    } = p;
    out.extend(registry.finish("registry"));
    out.extend(notes);
    let harness_config = harness_config_in(Some(&root), global_harness);
    let used = used
        .into_iter()
        .map(|h| (h, harness_config.clone()))
        .collect();
    Ok((out, used))
}

/// The blocker for a project nothing more can be checked of.
fn uncheckable(name: &str, entry: &ProjectEntry, main: &Path) -> Option<Finding> {
    if !path_utils::is_portable(&entry.root) {
        let mut finding = Finding::blocker(
            "registry",
            format!(
                "root \"{}\" is relative, so no host can resolve it",
                entry.root
            ),
            Some(format!("pm delete --project {name}")),
            &[Step::Repair],
        );
        finding
            .detail
            .push("then `pm register` it from its directory".to_string());
        return Some(finding);
    }
    if !main.is_dir() {
        let mut finding = Finding::blocker(
            "main/",
            format!("{} does not exist here", shell_path(main)),
            None,
            &[Step::Repair],
        );
        finding.detail.push(
            "run the check on the machine that has it, or leave it out with `--project`"
                .to_string(),
        );
        return Some(finding);
    }
    None
}

/// The blocker for agents with a live window.
fn running(agents: &Agents) -> Option<Finding> {
    if agents.running.is_empty() {
        return None;
    }
    let mut finding = Finding::blocker(
        "agents",
        format!(
            "{} running: {}",
            agents.running.len(),
            agents.running.join(", ")
        ),
        None,
        &[Step::StopAgents],
    );
    finding.detail.push(
        "they keep writing .pm/ and their conversations after the push and export; \
         `pm close --all` leaves them active, so the new host resumes them"
            .to_string(),
    );
    Some(finding)
}

struct Project<'a> {
    name: &'a str,
    entry: &'a ProjectEntry,
    root: &'a Path,
    main: &'a Path,
    features: &'a [(String, FeatureState)],
    out: Vec<Finding>,
    notes: Vec<Finding>,
    /// The registry entry's missing fields.
    registry: Line,
}

impl Project<'_> {
    fn directory_note(&mut self) {
        let dir = self.root.file_name().unwrap_or_default().to_string_lossy();
        if dir != self.name {
            self.notes.push(Finding::note(
                "",
                format!(
                    "the registry names it '{}' but its directory is '{dir}': `pm restore` \
                     registers a clone under the directory name",
                    self.name
                ),
            ));
        }
    }

    fn branch<'b>(
        &'b self,
        name: &'b str,
        worktree: Option<&'b Path>,
        feature: bool,
    ) -> Branch<'b> {
        Branch {
            root: self.root,
            main: self.main,
            name,
            worktree,
            feature,
        }
    }

    /// main's, each feature's, and each outside base's branch and worktree,
    /// and the registry's repo_url.
    fn branches(&mut self, agents: &Agents) -> Result<()> {
        let origin = git::remote_url(self.main, "origin")?;
        match (&self.entry.repo_url, &origin) {
            (None, _) => {
                self.registry.problem("no repo_url");
                self.registry.detail("`pm restore` clones from repo_url");
                self.registry.steps(BACKFILL);
            }
            (Some(url), Some(actual)) if url != actual => self.notes.push(Finding::note(
                "registry",
                format!("clones {url} but origin is {actual}"),
            )),
            _ => {}
        }
        let remote = Remote::of(self.main)?;
        let main_branch = &self.entry.main_branch;
        let mut checked = worktree::check(
            &self.branch(main_branch, Some(self.main), false),
            remote.as_ref(),
        )?;
        if origin.is_none() {
            checked.line.problem("no origin, so nothing to clone from");
            checked.line.git("remote add origin <url>");
            checked.line.git(format!("push -u origin {main_branch}"));
            checked
                .line
                .steps(&[Step::Branches, Step::Backfill, Step::GlobalPush]);
        }
        let mut lines = vec![("main/".to_string(), checked, false)];

        let branches: Vec<&str> = self
            .features
            .iter()
            .map(|(_, f)| f.branch.as_str())
            .collect();
        let mut bases: Vec<&str> = Vec::new();
        for (feature, state) in self.features {
            if state.status == FeatureStatus::Merged {
                continue;
            }
            let base = state.base.as_str();
            if base != main_branch && !branches.contains(&base) && !bases.contains(&base) {
                bases.push(base);
                let checked = worktree::check(&self.branch(base, None, false), remote.as_ref())?;
                lines.push((format!("{base} (base)"), checked, false));
                self.notes.push(Finding::note(
                    base,
                    format!(
                        "features stack on it, but it is no feature's branch: on the new host \
                         it is origin/{base}, with no local branch"
                    ),
                ));
            }
            if !state.status.is_active() {
                self.notes.push(Finding::note(
                    feature,
                    format!(
                        "{}: `pm restore` recreates no worktree for it",
                        state.status
                    ),
                ));
            }
            let path = self.root.join(&state.worktree);
            let present = path.is_dir();
            let checked = worktree::check(
                &self.branch(&state.branch, present.then_some(path.as_path()), true),
                remote.as_ref(),
            )?;
            let subject = if present {
                format!("{}/", rel_path(self.root, &path))
            } else {
                feature.clone()
            };
            let in_flight = checked.dirty && agents.active_in(feature) > 0;
            lines.push((subject, checked, in_flight));
        }
        for (subject, checked, in_flight) in lines {
            if checked.behind {
                self.notes.push(Finding::note(
                    &subject,
                    "origin is ahead of the local branch; the new host gets origin's".to_string(),
                ));
            }
            if let Some(mut finding) = checked.line.finish(&subject) {
                finding.in_flight = in_flight;
                self.out.push(finding);
            }
        }
        Ok(())
    }

    /// The `.pm/` state repo, and the registry's state_remote.
    fn state(&mut self, pm_dir: &Path) -> Result<()> {
        let mut state = Line::default();
        if !git::is_git_repo(pm_dir) {
            state.problem("not a git repo");
            state.steps(&[
                Step::StateInit,
                Step::StatePush,
                Step::Backfill,
                Step::GlobalPush,
            ]);
            self.out.extend(state.finish(".pm/"));
            return Ok(());
        }
        let remote = Remote::of(pm_dir)?;
        match (&self.entry.state_remote, &remote) {
            (_, None) => {
                state.problem("no remote");
                state.detail("its features, agents and messages stay here");
                state.steps(&[
                    Step::StateRemote,
                    Step::StatePush,
                    Step::Backfill,
                    Step::GlobalPush,
                ]);
            }
            (None, Some(_)) => {
                self.registry.problem("no state_remote");
                self.registry
                    .detail("`pm restore` pulls .pm/ from state_remote");
                self.registry.steps(BACKFILL);
            }
            _ => {}
        }
        if crate::commands::docs::would_migrate_docs_submodule(self.root) {
            state.problem("docs/ is a nested git repo `pm state push` doesn't carry");
            state.command("pm upgrade --all");
            state.steps(&[Step::Repair, Step::StatePush]);
        }
        let note = repo::state_repo(
            &mut state,
            pm_dir,
            ".pm",
            ".pm/",
            remote.as_ref(),
            Step::StatePush,
            "pm state pull",
        )?;
        self.out.extend(state.finish(".pm/"));
        self.notes.extend(note);
        Ok(())
    }

    /// Harness settings files git doesn't track, for each harness `used`.
    fn settings(&mut self, used: &[Harness]) -> Result<()> {
        let mut worktrees = vec![self.main.to_path_buf()];
        worktrees.extend(
            self.features
                .iter()
                .filter(|(_, f)| f.status.is_active())
                .map(|(_, f)| self.root.join(&f.worktree)),
        );
        for harness in used {
            self.out.extend(settings_findings(
                self.root, self.name, *harness, &worktrees,
            )?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::Severity;
    use super::*;
    use crate::state::agent::AgentRegistry;
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
        let about = |subject: &str| {
            found
                .iter()
                .find(|f| f.subject == subject)
                .unwrap_or_else(|| panic!("no finding about {subject}: {found:#?}"))
        };
        let registry = about("registry");
        assert_eq!(registry.what, "no repo_url");
        assert!(registry.steps.contains(&Step::Backfill));
        let login = about("login/");
        assert_eq!(login.what, "1 commit not pushed");
        assert_eq!(
            login.fix.as_deref(),
            Some("git -C login push -u origin login")
        );
        // An untracked file may be a secret: the fix never adds everything.
        let main = about("main/");
        assert_eq!(main.what, "1 untracked");
        assert!(!main.fix.as_ref().unwrap().contains("add -A"));
        assert!(main.detail.iter().any(|d| d.contains("notes.txt")));
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
            .find(|f| f.subject == "login/")
            .unwrap_or_else(|| panic!("{found:#?}"));
        assert_eq!(diverged.what, "diverged from origin");
        assert_eq!(
            diverged.fix.as_deref(),
            Some("(cd login && git pull --rebase origin login && git push -u origin login)")
        );
    }

    #[test]
    fn a_features_problems_are_one_line_with_one_fix_and_wip_is_in_flight() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (root, name) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&root);
        let repo = bare(&dir.path().join("repo.git"));
        git::add_remote(&main, "origin", &repo).unwrap();
        git::push(&main, "origin", "main").unwrap();
        let login = root.join("login");
        std::fs::write(login.join("a.txt"), "a").unwrap();
        git::add_all(&login).unwrap();
        git::commit_with_message(&login, "a").unwrap();
        std::fs::write(login.join("a.txt"), "changed").unwrap();
        std::fs::write(login.join("b.txt"), "b").unwrap();
        let entry = ProjectEntry {
            root: path_utils::to_portable(&root),
            main_branch: "main".to_string(),
            repo_url: Some(repo),
            state_remote: None,
        };
        let login_line = || {
            blockers(&name, &entry)
                .into_iter()
                .find(|f| f.subject == "login/")
                .unwrap()
        };
        let finding = login_line();
        assert_eq!(finding.what, "1 modified, 1 untracked, not on origin");
        assert_eq!(
            finding.fix.as_deref(),
            Some("(cd login && git add <file>… && git commit -a && git push -u origin login)")
        );
        assert_eq!(finding.steps, [Step::Branches]);
        assert!(!finding.in_flight);

        let agents = paths::agents_dir(&root);
        let mut registry = AgentRegistry::load(&agents, "login").unwrap();
        let mut agent: crate::state::agent::AgentEntry =
            toml::from_str("type = \"agent\"").unwrap();
        agent.active = true;
        registry.register("implementer", agent);
        registry.save(&agents, "login").unwrap();
        assert!(login_line().in_flight);
    }

    #[test]
    fn a_rebase_in_progress_is_resolved_before_anything_is_committed() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (root, name) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let main = paths::main_worktree(&root);
        let login = root.join("login");
        for (worktree, text) in [(&main, "main"), (&login, "login")] {
            std::fs::write(worktree.join("c.txt"), text).unwrap();
            git::add_all(worktree).unwrap();
            git::commit_with_message(worktree, text).unwrap();
        }
        assert!(git::run_git(&login, &["rebase", "main"]).is_err());
        let entry = ProjectEntry {
            root: path_utils::to_portable(&root),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        let found = blockers(&name, &entry);
        let rebase = found.iter().find(|f| f.subject == "login/").unwrap();
        assert_eq!(rebase.what, "rebase in progress");
        assert_eq!(
            rebase.fix.as_deref(),
            Some("git -C login rebase --continue")
        );
        assert_eq!(rebase.steps, [Step::Repair]);
    }

    #[test]
    fn json_carries_the_verdict_and_where_each_step_runs() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (root, name) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
        let entry = ProjectEntry {
            root: path_utils::to_portable(&root),
            main_branch: "main".to_string(),
            repo_url: None,
            state_remote: None,
        };
        let (findings, _) = findings(&name, &entry, None, &HarnessConfig::default()).unwrap();
        let report = super::super::Report {
            sections: vec![super::super::Section {
                kind: super::super::SectionKind::Project,
                name: name.clone(),
                path: entry.root.clone(),
                findings,
            }],
        };
        let json = report.json();
        assert_eq!(json["ready"], false);
        let push = json["plan"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["step"] == "state-push")
            .unwrap();
        assert_eq!(push["command"], "pm state push");
        assert_eq!(push["projects"], serde_json::json!([name]));
    }
}
