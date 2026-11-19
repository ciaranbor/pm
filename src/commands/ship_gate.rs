//! The extra condition on `pm feat status ready` in a project that ships
//! features as GitHub PRs (`[github] ship`): the feature's work is in its
//! PR and the PR's checks pass. A feature with nothing to ship (no commits
//! ahead of its base, clean worktree) and a `pm feat review` feature, whose
//! PR is someone else's, pass unchecked.

use std::path::Path;

use super::feat_review::REVIEW_WORKFLOW;
use crate::error::{PmError, Result};
use crate::gh::{self, Outcome};
use crate::git;
use crate::state::feature::FeatureState;
use crate::state::{paths, project};

/// How many uncommitted paths a refusal names before eliding the rest.
const NAMED_PATHS: usize = 5;

/// Whether a feature running `workflow` ships its work as a PR: the
/// project ships, and the feature isn't reviewing someone else's PR.
pub fn applies(project_root: &Path, workflow: Option<&str>) -> bool {
    workflow != Some(REVIEW_WORKFLOW) && project::ships_to_github(project_root)
}

/// `Ok` if `feature` (named `name`) may be marked ready as far as shipping
/// goes.
pub fn check(project_root: &Path, name: &str, feature: &FeatureState) -> Result<()> {
    if !applies(project_root, feature.workflow.as_deref()) {
        return Ok(());
    }
    let refuse = |why: String| Err(PmError::SafetyCheck(format!("feature '{name}' {why}")));
    let worktree = project_root.join(&feature.worktree);

    let changed = git::changed_paths(&worktree)?;
    if !changed.is_empty() {
        let mut named: Vec<&str> = changed
            .iter()
            .take(NAMED_PATHS)
            .map(String::as_str)
            .collect();
        if changed.len() > NAMED_PATHS {
            named.push("…");
        }
        return refuse(format!(
            "has uncommitted changes; commit them first:\n  {}",
            named.join("\n  ")
        ));
    }

    let main_branch = git::main_branch(&paths::main_worktree(project_root))?;
    let base = feature.base_branch(&main_branch);
    // A fresh feature starts from the base's remote copy, which the local
    // base may lag: commits only there are not the feature's own.
    let remote = git::branch_remote(&worktree, base)?.unwrap_or_else(|| "origin".to_string());
    let remote_base = format!("{remote}/{base}");
    let mut bases = vec![base];
    if git::ref_exists(&worktree, &format!("refs/remotes/{remote_base}"))? {
        bases.push(&remote_base);
    }
    if git::commits_beyond(&worktree, &feature.branch, &bases)? == 0 {
        return Ok(());
    }

    if feature.pr.is_empty() {
        return refuse(
            "has no PR; open one with `pm feat pr create --ready --body <file>`".to_string(),
        );
    }
    let pr = gh::pr_checks(&worktree, &feature.pr)?;
    if pr.state == "CLOSED" {
        return refuse(format!("has PR #{}, which is closed", feature.pr));
    }
    let head = git::branch_commit(&worktree, &feature.branch)?;
    if head.as_deref() != Some(pr.head_oid.as_str()) {
        return refuse(format!(
            "has commits PR #{} doesn't; push them (`git push`) first",
            feature.pr
        ));
    }
    let failing = pr.named(Outcome::Fail);
    if !failing.is_empty() {
        return refuse(format!(
            "has failing checks on PR #{}: {}; see `gh run view --log-failed`",
            feature.pr,
            failing.join(", ")
        ));
    }
    let pending = pr.named(Outcome::Pending);
    if !pending.is_empty() {
        return refuse(format!(
            "has checks still running on PR #{}: {}; wait with `gh pr checks --watch`",
            feature.pr,
            pending.join(", ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_status::feat_status;
    use crate::gh::fake;
    use crate::state::feature::Progress;
    use crate::state::project::ProjectConfig;
    use crate::state::scope::TeamStatus;
    use crate::testing::TestServer;
    use std::path::PathBuf;
    use tempfile::{TempDir, tempdir};

    struct Fixture {
        dir: TempDir,
        project: PathBuf,
    }

    impl Fixture {
        /// A project with feature `login`, shipping when `ship`.
        fn new(ship: bool) -> Self {
            let dir = tempdir().unwrap();
            let (project, _, _) =
                TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
            let pm_dir = paths::pm_dir(&project);
            let mut config = ProjectConfig::load(&pm_dir).unwrap();
            config.github.ship = Some(ship);
            config.save(&pm_dir).unwrap();
            Self { dir, project }
        }

        fn feature(&self) -> FeatureState {
            FeatureState::load(&paths::features_dir(&self.project), "login").unwrap()
        }

        fn set_feature(&self, edit: impl FnOnce(&mut FeatureState)) {
            let mut state = self.feature();
            edit(&mut state);
            state
                .save(&paths::features_dir(&self.project), "login")
                .unwrap();
        }

        fn commit(&self) -> String {
            TestServer::add_feature_commit(&self.project, "login");
            git::branch_commit(&self.project.join("login"), "login")
                .unwrap()
                .unwrap()
        }

        fn check(&self) -> Result<()> {
            check(&self.project, "login", &self.feature())
        }
    }

    /// A `gh pr view` reply for a PR at `head` with `rollup` as its checks.
    fn pr_reply(head: &str, rollup: &str) -> String {
        format!(r#"{{"state":"OPEN","headRefOid":"{head}","statusCheckRollup":[{rollup}]}}"#)
    }

    fn refusal(result: Result<()>) -> String {
        match result {
            Err(PmError::SafetyCheck(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_project_that_does_not_ship_needs_no_pr() {
        let fx = Fixture::new(false);
        fx.commit();
        let gh = fake::install(|args| panic!("unexpected gh call: {args:?}"));
        fx.check().unwrap();
        assert!(gh.calls().is_empty());
    }

    #[test]
    fn a_feature_with_nothing_to_ship_passes_without_asking_github() {
        let fx = Fixture::new(true);
        let gh = fake::install(|args| panic!("unexpected gh call: {args:?}"));
        fx.check().unwrap();
        assert!(gh.calls().is_empty());
    }

    #[test]
    fn a_review_feature_passes_though_its_pr_is_not_green() {
        let fx = Fixture::new(true);
        fx.commit();
        fx.set_feature(|s| s.workflow = Some(REVIEW_WORKFLOW.to_string()));
        let gh = fake::install(|args| panic!("unexpected gh call: {args:?}"));
        fx.check().unwrap();
        assert!(gh.calls().is_empty());
    }

    #[test]
    fn a_fresh_feature_with_no_commits_of_its_own_passes_though_local_main_lags() {
        let fx = Fixture::new(true);
        let main = paths::main_worktree(&fx.project);
        let origin = fx.dir.path().join("origin.git");
        let run = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git::init_bare(&origin).unwrap();
        run(
            &main,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        run(&main, &["commit", "--allow-empty", "-m", "upstream"]);
        run(&main, &["push", "-u", "origin", "main"]);
        run(&main, &["reset", "--hard", "HEAD~1"]);
        run(
            &fx.project.join("login"),
            &["reset", "--hard", "origin/main"],
        );

        let gh = fake::install(|args| panic!("unexpected gh call: {args:?}"));
        fx.check().unwrap();
        assert!(gh.calls().is_empty());
    }

    #[test]
    fn a_closed_pr_is_refused() {
        let fx = Fixture::new(true);
        let head = fx.commit();
        fx.set_feature(|s| s.pr = "7".to_string());
        let _gh =
            fake::install(move |_| Ok(pr_reply(&head, "").replace(r#""OPEN""#, r#""CLOSED""#)));
        let why = refusal(fx.check());
        assert!(why.contains("PR #7, which is closed"), "{why}");
    }

    #[test]
    fn uncommitted_work_is_refused_and_named() {
        let fx = Fixture::new(true);
        std::fs::write(fx.project.join("login/new.txt"), "x").unwrap();
        let why = refusal(fx.check());
        assert!(why.contains("commit them first"), "{why}");
        assert!(why.contains("new.txt"), "{why}");
    }

    #[test]
    fn committed_work_without_a_pr_is_refused() {
        let fx = Fixture::new(true);
        fx.commit();
        let why = refusal(fx.check());
        assert!(why.contains("pm feat pr create --ready"), "{why}");
    }

    #[test]
    fn a_pr_behind_the_branch_is_refused() {
        let fx = Fixture::new(true);
        fx.commit();
        fx.set_feature(|s| s.pr = "7".to_string());
        let _gh = fake::install(|_| Ok(pr_reply("0000000", "")));
        let why = refusal(fx.check());
        assert!(why.contains("git push"), "{why}");
    }

    #[test]
    fn failing_checks_are_refused_and_named_before_pending_ones() {
        let fx = Fixture::new(true);
        let head = fx.commit();
        fx.set_feature(|s| s.pr = "7".to_string());
        let _gh = fake::install(move |_| {
            Ok(pr_reply(
                &head,
                r#"{"name":"test","status":"COMPLETED","conclusion":"FAILURE"},
                {"name":"slow","status":"IN_PROGRESS","conclusion":""}"#,
            ))
        });
        let why = refusal(fx.check());
        assert!(why.contains("failing checks on PR #7: test;"), "{why}");
        assert!(why.contains("gh run view --log-failed"), "{why}");
    }

    #[test]
    fn pending_checks_are_refused_and_named() {
        let fx = Fixture::new(true);
        let head = fx.commit();
        fx.set_feature(|s| s.pr = "7".to_string());
        let _gh = fake::install(move |_| {
            Ok(pr_reply(
                &head,
                r#"{"name":"lint","status":"COMPLETED","conclusion":"SUCCESS"},
                {"context":"ci/wait","state":"PENDING"}"#,
            ))
        });
        let why = refusal(fx.check());
        assert!(why.contains("still running on PR #7: ci/wait;"), "{why}");
    }

    #[test]
    fn a_pushed_pr_with_passing_or_no_checks_passes() {
        let fx = Fixture::new(true);
        let head = fx.commit();
        fx.set_feature(|s| s.pr = "7".to_string());
        for rollup in [
            "",
            r#"{"name":"lint","status":"COMPLETED","conclusion":"SUCCESS"},
            {"name":"docs","status":"COMPLETED","conclusion":"SKIPPED"}"#,
        ] {
            let reply = pr_reply(&head, rollup);
            let _gh = fake::install(move |_| Ok(reply.clone()));
            fx.check().unwrap();
        }
    }

    #[test]
    fn a_gh_failure_refuses_with_its_error() {
        let fx = Fixture::new(true);
        fx.commit();
        fx.set_feature(|s| s.pr = "7".to_string());
        let _gh = fake::install(|_| Err(PmError::Gh("HTTP 401: Bad credentials".to_string())));
        let err = fx.check().unwrap_err();
        assert!(err.to_string().contains("Bad credentials"), "{err}");
    }

    #[test]
    fn a_refused_ready_keeps_the_status_and_tells_main_nothing() {
        let fx = Fixture::new(true);
        fx.commit();
        std::fs::write(
            crate::commands::feat_summary::path(&fx.project, "login").unwrap(),
            "notes",
        )
        .unwrap();

        refusal(
            feat_status(
                &fx.project,
                "login",
                Progress::Ready,
                None,
                Some("implementer"),
                crate::testing::NO_SERVER,
            )
            .map(|_| ()),
        );

        assert_eq!(
            TeamStatus::load(&fx.project, "login").unwrap().progress,
            Progress::Wip
        );
        let inbox =
            crate::messages::check(&paths::messages_dir(&fx.project), "main", "main").unwrap();
        assert!(inbox.is_empty());
    }
}
