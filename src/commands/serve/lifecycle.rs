//! The lifecycle endpoints: merge and delete a feature, restart an agent,
//! through the handlers the CLI runs. A handler's refusal reaches the
//! device as the CLI words it.
//!
//! Each runs to its end within the request, whether or not the device is
//! still there to hear how it went. The post-merge hook doesn't hold it:
//! it runs in the base session's `hook` window, in the shell tmux starts
//! there, so it has the user's environment rather than the server's. A
//! restart leaves which window each session shows alone, so no tmux client
//! moves for it; merge and delete move only a client viewing the feature's
//! session, which they kill, as the CLI does.

use std::path::Path;

use crate::commands::{agent_restart, feat_delete, feat_merge, tmux_push};
use crate::error::{PmError, Result};

use super::Config;
use super::input::Written;
use super::routes::{error, json};
use super::transcript::Agent;

/// `POST features/{project}/{feature}/{merge|delete}` for the feature
/// `feature` of the project at `root`.
pub(super) fn feature(
    config: &Config,
    root: &Path,
    feature: &str,
    action: &str,
) -> Result<Written> {
    let server = config.tmux_server.as_deref();
    let done = match action {
        "merge" => feat_merge::feat_merge(root, &config.projects_dir, feature, false, server)
            .map(|()| serde_json::json!({ "merged": true })),
        "delete" => feat_delete::feat_delete(root, &config.projects_dir, feature, false, server)
            .map(|()| serde_json::json!({ "deleted": true })),
        _ => {
            return Ok(Written {
                reply: error(404, "no such endpoint"),
                detail: String::new(),
            });
        }
    };
    finish(config, action, done)
}

/// A restart's body; an empty one is `{}`.
#[derive(serde::Deserialize)]
struct RestartBody {
    #[serde(default)]
    force: bool,
}

/// `POST agents/{project}/{scope}/{agent}/restart`, `{"force"}` optional:
/// respawn the agent, resuming its session; an agent mid-turn is refused
/// with `mid-turn` unless `force`, and then told to resume.
pub(super) fn restart(config: &Config, agent: &Agent, body: &str) -> Result<Written> {
    let body = if body.trim().is_empty() { "{}" } else { body };
    let Ok(RestartBody { force }) = serde_json::from_str(body) else {
        return Ok(Written {
            reply: error(400, "the body is {\"force\": true|false} or empty"),
            detail: String::new(),
        });
    };
    let server = config.tmux_server.as_deref();
    let names = [agent.name.clone()];
    let mut restarted =
        agent_restart::agent_restart_many(&agent.root, &agent.scope, &names, force, false, server);
    restarted.confirm_launches(&agent.root, &agent.scope, server);
    let done = std::mem::take(&mut restarted.results)
        .pop()
        .unwrap_or_else(|| Err(PmError::Agent("nothing was restarted".into())))
        .map(|line| serde_json::json!({ "restarted": line }));
    restarted.finish(server);
    let action = if force { "restart --force" } else { "restart" };
    finish(config, action, done)
}

/// The reply to an action that ended `done`. A refusal, which changed
/// nothing, is a `409`; any other failure may have come partway, so it is
/// an error the device reads as such. Either way tmux and the poller are
/// told, since state may have changed.
fn finish(config: &Config, action: &str, done: Result<serde_json::Value>) -> Result<Written> {
    let _ = tmux_push::push(&config.projects_dir, config.tmux_server.as_deref());
    super::wake(&config.devices);
    let reply = match done {
        Ok(body) => json(200, body),
        Err(e) => {
            let refused = match &e {
                PmError::SafetyCheck(_) if action.starts_with("restart") => "mid-turn",
                PmError::SafetyCheck(_) => "unsafe",
                PmError::MergeAborted(_) => "conflict",
                _ => return Err(e),
            };
            json(
                409,
                serde_json::json!({ "error": e.to_string(), "refused": refused }),
            )
        }
    };
    Ok(Written {
        reply,
        detail: action.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{call, fixture, pair};
    use crate::state::feature::FeatureState;
    use crate::state::paths;
    use crate::testing::TestServer;
    use crate::tmux;

    fn body(reply: &str) -> serde_json::Value {
        serde_json::from_str(reply).unwrap()
    }

    #[test]
    fn a_merge_is_refused_as_the_cli_words_it_until_the_work_is_committed() {
        let f = fixture();
        let phone = pair(&f.config, "phone");
        let merge = format!("/v1/features/{}/login/merge", f.project_name);
        TestServer::add_feature_commit(&f.project, "login");
        std::fs::write(f.project.join("login/wip.txt"), "draft").unwrap();
        crate::git::run_git(&f.project.join("login"), &["add", "wip.txt"]).unwrap();

        let (status, reply) = call(&f.config, "POST", &merge, &phone, "");
        assert_eq!(status, 409, "{reply}");
        assert_eq!(
            body(&reply),
            serde_json::json!({
                "error": "feature 'login' has uncommitted changes — commit or stash before merging",
                "refused": "unsafe",
            })
        );

        crate::git::run_git(&f.project.join("login"), &["commit", "-qm", "wip"]).unwrap();
        let (status, reply) = call(&f.config, "POST", &merge, &phone, "");
        assert_eq!(
            (status, body(&reply)),
            (200, serde_json::json!({"merged": true}))
        );
        assert!(paths::main_worktree(&f.project).join("wip.txt").exists());
        assert!(FeatureState::load(&paths::features_dir(&f.project), "login").is_err());
        let told =
            crate::messages::read_at(&paths::messages_dir(&f.project), "main", "main", "login", 1)
                .unwrap()
                .unwrap();
        assert!(told.body.contains("'login' was merged"), "{}", told.body);
    }

    #[test]
    fn a_delete_of_unmerged_work_is_refused() {
        let f = fixture();
        let phone = pair(&f.config, "phone");
        TestServer::add_feature_commit(&f.project, "login");
        let ghost = format!("/v1/features/{}/ghost/delete", f.project_name);
        let (status, reply) = call(&f.config, "POST", &ghost, &phone, "");
        assert_eq!(
            (status, body(&reply)["error"].as_str()),
            (404, Some("no such feature"))
        );

        let delete = format!("/v1/features/{}/login/delete", f.project_name);
        let (status, reply) = call(&f.config, "POST", &delete, &phone, "");
        assert_eq!(status, 409, "{reply}");
        assert_eq!(
            body(&reply)["error"],
            "feature 'login' has commits not merged into its base. Use --force to override."
        );
        assert!(FeatureState::load(&paths::features_dir(&f.project), "login").is_ok());
    }

    #[test]
    fn a_restart_refuses_a_mid_turn_agent_unless_forced_and_moves_no_client() {
        let f = fixture();
        let phone = pair(&f.config, "phone");
        let session = tmux::session_name(&f.project_name, "login");
        f.server
            .spawn_fake_agent(&f.project, &session, "login", "implementer");
        let shell =
            tmux::new_window(f.server.name(), &session, &f.project, Some("shell"), true).unwrap();
        tmux::select_window(f.server.name(), &shell).unwrap();
        let shown = || tmux::active_window_name(f.server.name(), &session).unwrap();
        let restart = format!("/v1/agents/{}/login/implementer/restart", f.project_name);

        let (status, reply) = call(&f.config, "POST", &restart, &phone, "");
        assert_eq!(status, 409, "{reply}");
        assert_eq!(body(&reply)["refused"], "mid-turn");

        let (status, reply) = call(&f.config, "POST", &restart, &phone, r#"{"force": true}"#);
        assert_eq!(status, 200, "{reply}");
        assert!(
            body(&reply)["restarted"]
                .as_str()
                .unwrap()
                .starts_with("Restarted agent 'implementer'"),
            "{reply}"
        );
        assert_eq!(shown().as_deref(), Some("shell"), "no client is moved");
        assert_eq!(
            crate::messages::list(
                &paths::messages_dir(&f.project),
                "login",
                "implementer",
                None
            )
            .unwrap()
            .len(),
            1,
            "told to resume"
        );
    }

    #[test]
    fn a_conflicting_merge_is_refused_aborted_and_leaves_the_feature() {
        let f = fixture();
        let phone = pair(&f.config, "phone");
        let main = paths::main_worktree(&f.project);
        for (repo, text) in [(main.clone(), "main"), (f.project.join("login"), "feature")] {
            std::fs::write(repo.join("shared.txt"), text).unwrap();
            crate::git::run_git(&repo, &["add", "shared.txt"]).unwrap();
            crate::git::run_git(&repo, &["commit", "-qm", text]).unwrap();
        }

        let merge = format!("/v1/features/{}/login/merge", f.project_name);
        let (status, reply) = call(&f.config, "POST", &merge, &phone, "");
        assert_eq!(status, 409, "{reply}");
        let reply = body(&reply);
        assert_eq!(reply["refused"], "conflict");
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("merge was aborted"),
            "{reply}"
        );
        assert!(!crate::git::has_uncommitted_changes(&main).unwrap());
        assert!(FeatureState::load(&paths::features_dir(&f.project), "login").is_ok());
    }
}
