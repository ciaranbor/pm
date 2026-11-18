//! A feature with a linked PR: merged on GitHub (`gh` faked, the remote a
//! bare repo), refused while GitHub would not or should not merge it, and
//! only cleaned up once GitHub has merged it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use super::*;
use crate::gh::fake;
use crate::git::run_git;
use crate::state::project::ProjectConfig;

/// The PR as the fake GitHub holds it.
#[derive(Clone)]
struct Pr {
    state: &'static str,
    draft: bool,
    mergeable: &'static str,
    head: String,
    base: &'static str,
    failing_check: Option<&'static str>,
    /// What `gh pr merge` does: land the PR (squashed into the remote's
    /// main), queue it without landing, or land it and still fail.
    on_merge: OnMerge,
}

/// A change to an open PR that should stop its merge.
type Break = fn(&mut Pr);

#[derive(Clone, Copy, PartialEq)]
enum OnMerge {
    Land,
    Queue,
    LandThenFail,
}

struct Hub {
    project: PathBuf,
    registry: PathBuf,
    main: PathBuf,
    remote: PathBuf,
    pr: Rc<RefCell<Pr>>,
    /// Where the fake clones the remote to land a merge.
    scratch: PathBuf,
    _dir: tempfile::TempDir,
}

/// Project with feature "login", one commit ahead and pushed to `origin`,
/// linked to PR #7 on a fake GitHub that allows `allowed` merge methods and
/// preselects squash.
fn hub(server: &TestServer, allowed: &'static [&'static str]) -> (Hub, fake::Fake) {
    let dir = tempdir().unwrap();
    let (project, registry, _) = server.setup_project_with_feature_no_tmux(dir.path(), "login");
    let main = paths::main_worktree(&project);
    let remote = dir.path().join("remote.git");
    git::init_bare(&remote).unwrap();
    git::add_remote(&main, "origin", &remote.to_string_lossy()).unwrap();
    git::push(&main, "origin", "main").unwrap();
    TestServer::add_feature_commit(&project, "login");
    git::push(&project.join("login"), "origin", "login").unwrap();
    let features = paths::features_dir(&project);
    let mut state = FeatureState::load(&features, "login").unwrap();
    state.pr = "7".to_string();
    state.save(&features, "login").unwrap();

    let pr = Rc::new(RefCell::new(Pr {
        state: "OPEN",
        draft: false,
        mergeable: "MERGEABLE",
        head: git::branch_commit(&main, "login").unwrap().unwrap(),
        base: "main",
        failing_check: None,
        on_merge: OnMerge::Land,
    }));
    let scratch = dir.path().join("scratch");
    let gh = fake::install({
        let pr = pr.clone();
        let remote = remote.clone();
        let scratch = scratch.clone();
        move |args| {
            let mut pr = pr.borrow_mut();
            match args {
                ["pr", "view", "7", ..] => Ok(serde_json::json!({
                    "state": pr.state,
                    "isDraft": pr.draft,
                    "mergeable": pr.mergeable,
                    "headRefOid": pr.head,
                    "baseRefName": pr.base,
                    "statusCheckRollup": pr.failing_check.map(|name| vec![serde_json::json!(
                        {"__typename": "CheckRun", "name": name, "conclusion": "FAILURE"}
                    )]).unwrap_or_default(),
                })
                .to_string()),
                ["repo", "view", ..] => Ok(serde_json::json!({
                    "mergeCommitAllowed": allowed.contains(&"merge"),
                    "squashMergeAllowed": allowed.contains(&"squash"),
                    "rebaseMergeAllowed": allowed.contains(&"rebase"),
                    "viewerDefaultMergeMethod": "SQUASH",
                })
                .to_string()),
                ["pr", "merge", "7", ..] => {
                    if pr.on_merge == OnMerge::Queue {
                        return Ok(String::new());
                    }
                    squash_on_remote(&remote, &scratch);
                    pr.state = "MERGED";
                    match pr.on_merge {
                        OnMerge::LandThenFail => Err(PmError::Gh(
                            "failed to run git: fatal: 'main' is already used by worktree"
                                .to_string(),
                        )),
                        _ => Ok(String::new()),
                    }
                }
                other => panic!("unexpected gh call: {other:?}"),
            }
        }
    });
    let hub = Hub {
        project,
        registry,
        main,
        remote,
        pr,
        scratch,
        _dir: dir,
    };
    (hub, gh)
}

/// Squash-merge `login` into `main` on the remote, as GitHub would: the
/// result shares no ancestry with the branch.
fn squash_on_remote(remote: &Path, scratch: &Path) {
    let parent = scratch.parent().unwrap();
    run_git(
        parent,
        &[
            "clone",
            &remote.to_string_lossy(),
            &scratch.to_string_lossy(),
        ],
    )
    .unwrap();
    run_git(scratch, &["merge", "--squash", "origin/login"]).unwrap();
    run_git(scratch, &["commit", "-m", "login (#7)"]).unwrap();
    run_git(scratch, &["push", "origin", "main"]).unwrap();
}

fn remote_has_branch(remote: &Path, branch: &str) -> bool {
    !run_git(remote, &["branch", "--list", branch])
        .unwrap()
        .is_empty()
}

fn merge_calls(gh: &fake::Fake) -> Vec<Vec<String>> {
    gh.calls()
        .into_iter()
        .filter(|call| call[..2] == ["pr", "merge"])
        .collect()
}

fn set_merge_method(project: &Path, method: &str) {
    let pm_dir = paths::pm_dir(project);
    let mut config = ProjectConfig::load(&pm_dir).unwrap();
    config.github.merge_method = Some(method.to_string());
    config.save(&pm_dir).unwrap();
}

#[test]
fn a_pr_merges_on_github_by_the_configured_method_then_main_is_pulled_and_the_feature_ends() {
    let server = TestServer::new();
    let (hub, gh) = hub(&server, &["merge", "squash", "rebase"]);
    set_merge_method(&hub.project, "rebase");
    let head = hub.pr.borrow().head.clone();

    let ended = feat_merge(&hub.project, &hub.registry, "login", false, server.name()).unwrap();

    assert_eq!(
        merge_calls(&gh),
        [[
            "pr",
            "merge",
            "7",
            "--rebase",
            "--match-head-commit",
            head.as_str()
        ]]
    );
    assert!(ended.warnings.is_empty(), "{:?}", ended.warnings);
    assert!(hub.main.join("feature.txt").exists(), "main was not pulled");
    assert!(!git::branch_exists(&hub.main, "login").unwrap());
    assert!(!remote_has_branch(&hub.remote, "login"));
    assert!(!hub.project.join("login").exists());
    assert!(!FeatureState::exists(
        &paths::features_dir(&hub.project),
        "login"
    ));
}

#[test]
fn a_pr_github_would_not_or_should_not_merge_is_refused_with_its_reason_and_nothing_changes() {
    let server = TestServer::new();
    let (hub, gh) = hub(&server, &["merge", "squash"]);
    let refusal = || {
        feat_merge(&hub.project, &hub.registry, "login", false, server.name())
            .unwrap_err()
            .to_string()
    };
    let cases: [(Break, &str); 6] = [
        (|pr| pr.state = "CLOSED", "is closed"),
        (|pr| pr.draft = true, "draft"),
        (|pr| pr.base = "develop", "targets 'develop'"),
        (|pr| pr.mergeable = "CONFLICTING", "conflicts with 'main'"),
        (
            |pr| pr.failing_check = Some("test"),
            "checks are failing: test",
        ),
        (|pr| pr.head = "0".repeat(40), "push the branch"),
    ];
    let open = hub.pr.borrow().clone();
    for (break_pr, reason) in cases {
        *hub.pr.borrow_mut() = open.clone();
        break_pr(&mut hub.pr.borrow_mut());
        let err = refusal();
        assert!(err.contains(reason), "expected '{reason}' in: {err}");
    }

    *hub.pr.borrow_mut() = open;
    set_merge_method(&hub.project, "rebase");
    let err = refusal();
    assert!(
        err.contains("does not allow") && err.contains("merge, squash"),
        "{err}"
    );
    set_merge_method(&hub.project, "fast-forward");
    assert!(refusal().contains("must be squash, merge or rebase"));

    assert!(merge_calls(&gh).is_empty());
    assert!(hub.project.join("login").exists());
    assert!(remote_has_branch(&hub.remote, "login"));
    let state = FeatureState::load(&paths::features_dir(&hub.project), "login").unwrap();
    assert_ne!(state.status, FeatureStatus::Merged);
}

#[test]
fn a_pr_already_merged_on_github_is_only_cleaned_up_though_git_cannot_see_the_squash() {
    let server = TestServer::new();
    let (hub, gh) = hub(&server, &["squash"]);
    squash_on_remote(&hub.remote, &hub.scratch);
    hub.pr.borrow_mut().state = "MERGED";

    feat_merge(&hub.project, &hub.registry, "login", false, server.name()).unwrap();

    assert!(merge_calls(&gh).is_empty());
    assert!(hub.main.join("feature.txt").exists(), "main was not pulled");
    assert!(!hub.project.join("login").exists());
    assert!(!remote_has_branch(&hub.remote, "login"));
}

#[test]
fn a_merge_github_only_queued_ends_nothing_and_a_retry_once_it_lands_cleans_up() {
    let server = TestServer::new();
    let (hub, gh) = hub(&server, &["squash"]);
    hub.pr.borrow_mut().on_merge = OnMerge::Queue;

    let err = feat_merge(&hub.project, &hub.registry, "login", false, server.name())
        .unwrap_err()
        .to_string();
    assert!(err.contains("has not merged it yet"), "{err}");
    assert!(hub.project.join("login").exists());

    hub.pr.borrow_mut().on_merge = OnMerge::LandThenFail;
    feat_merge(&hub.project, &hub.registry, "login", false, server.name()).unwrap();

    assert_eq!(merge_calls(&gh).len(), 2);
    assert_eq!(merge_calls(&gh)[1][3], "--squash");
    assert!(hub.main.join("feature.txt").exists());
    assert!(!hub.project.join("login").exists());
}

#[test]
fn a_pr_merged_on_github_without_the_local_branch_s_later_commits_is_not_cleaned_up() {
    let server = TestServer::new();
    let (hub, _gh) = hub(&server, &["squash"]);
    squash_on_remote(&hub.remote, &hub.scratch);
    hub.pr.borrow_mut().state = "MERGED";
    let worktree = hub.project.join("login");
    std::fs::write(worktree.join("later.txt"), "later").unwrap();
    git::stage_file(&worktree, "later.txt").unwrap();
    git::commit(&worktree, "later").unwrap();

    let err = feat_merge(&hub.project, &hub.registry, "login", false, server.name())
        .unwrap_err()
        .to_string();

    assert!(err.contains("later commits would be lost"), "{err}");
    assert!(worktree.exists());
    assert!(git::branch_exists(&hub.main, "login").unwrap());
}

#[test]
fn a_remote_branch_github_already_deleted_is_no_warning() {
    let server = TestServer::new();
    let (hub, _gh) = hub(&server, &["squash"]);
    run_git(&hub.remote, &["branch", "-D", "login"]).unwrap();
    hub.pr.borrow_mut().state = "MERGED";

    let ended = feat_merge(&hub.project, &hub.registry, "login", false, server.name()).unwrap();

    assert!(ended.warnings.is_empty(), "{:?}", ended.warnings);
    assert!(!hub.project.join("login").exists());
}

#[test]
fn keep_merges_the_pr_but_keeps_the_feature_and_its_remote_branch() {
    let server = TestServer::new();
    let (hub, gh) = hub(&server, &["squash"]);

    feat_merge(&hub.project, &hub.registry, "login", true, server.name()).unwrap();

    assert_eq!(merge_calls(&gh).len(), 1);
    assert!(hub.project.join("login").exists());
    assert!(remote_has_branch(&hub.remote, "login"));
    let state = FeatureState::load(&paths::features_dir(&hub.project), "login").unwrap();
    assert_eq!(state.status, FeatureStatus::Merged);
}

#[test]
fn an_unset_method_falls_back_to_one_the_repo_allows_when_github_s_default_is_not() {
    let server = TestServer::new();
    let (hub, gh) = hub(&server, &["merge", "rebase"]);

    feat_merge(&hub.project, &hub.registry, "login", false, server.name()).unwrap();

    assert_eq!(merge_calls(&gh)[0][3], "--merge");
}
