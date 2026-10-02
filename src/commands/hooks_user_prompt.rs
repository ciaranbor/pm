//! `pm harness hooks user-prompt`: the user typing into an agent's window
//! answers a blocked feature, so the feature goes back to `wip`.
//!
//! Only the user's input resets it. pm messages arrive as Stop-hook
//! continuations, which no harness runs this hook for, or as the same text
//! typed in to re-arm an agent (see `agent_rearm`); that and
//! pm's own [`SPAWN_PROMPT`] are ignored. Blocked is per feature, so input
//! to any of its agents resets it.
//!
//! Any prompt, pm's own included, also means the agent is working again, so
//! it clears the agent's waiting marker ([`runtime`]), stamps its activity,
//! and has its window published busy, as the Stop hook does as a turn
//! resumes.
//!
//! The harness adds the hook's stdout to the model's context and may refuse
//! the prompt on a non-zero exit, so it prints nothing and always exits 0.
//! It runs on every prompt, so it does one state read and at most one write
//! beyond the marker and the stamp.

use std::io::Read;
use std::path::Path;

use crate::commands::agent_spawn::SPAWN_PROMPT;
use crate::commands::feat_status::feat_status;
use crate::commands::hooks_stop;
use crate::error::Result;
use crate::messages;
use crate::state::feature::{FeatureState, Progress};
use crate::state::paths;
use crate::state::runtime;

/// Run the hook. Always exit code 0, whatever happened. `on_prompt` gets
/// the agent's unread message count, and whether the feature was set back
/// to `wip` or the agent's marker cleared.
pub fn user_prompt(on_prompt: impl FnOnce(u32, bool)) -> i32 {
    if let Ok(Some((unread, changed))) = user_prompt_inner() {
        on_prompt(unread, changed);
    }
    0
}

fn user_prompt_inner() -> Result<Option<(u32, bool)>> {
    let Some(agent) = std::env::var("PM_AGENT_NAME")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return Ok(None);
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let Some(prompt) = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("prompt")?.as_str().map(str::to_string))
    else {
        return Ok(None);
    };
    let cwd = std::env::current_dir()?;
    let project_root = paths::find_project_root(&cwd)?;
    let scope = paths::resolve_scope_from(&project_root, &cwd)?;
    let changed = on_prompt(&project_root, &scope, &agent, &prompt)?;
    let unread = messages::unread_count(&paths::messages_dir(&project_root), &scope, &agent);
    Ok(Some((unread, changed)))
}

/// Any prompt to `agent`: clear its marker, and unblock its feature if the
/// prompt is the user's. Returns whether either changed.
fn on_prompt(project_root: &Path, scope: &str, agent: &str, prompt: &str) -> Result<bool> {
    runtime::touch_activity(project_root, scope, agent)?;
    let cleared = runtime::clear_waiting(project_root, scope, agent)?;
    Ok(on_user_prompt(project_root, scope, prompt)? || cleared)
}

/// Set `scope` back to `wip` if it is a blocked feature and `prompt` is the
/// user's. Returns whether it did.
pub(crate) fn on_user_prompt(project_root: &Path, scope: &str, prompt: &str) -> Result<bool> {
    if scope == "main" || prompt.trim() == SPAWN_PROMPT || hooks_stop::is_continuation(prompt) {
        return Ok(false);
    }
    let state = FeatureState::load(&paths::features_dir(project_root), scope)?;
    if state.progress != Progress::Blocked {
        return Ok(false);
    }
    feat_status(project_root, scope, Progress::Wip, None, None)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn state(project: &Path) -> FeatureState {
        FeatureState::load(&paths::features_dir(project), "login").unwrap()
    }

    fn blocked_feature(dir: &Path) -> std::path::PathBuf {
        let (project, _) = TestServer::new().setup_project_with_feature_no_tmux(dir, "login");
        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            Some("implementer"),
        )
        .unwrap();
        project
    }

    #[test]
    fn the_users_prompt_unblocks_the_feature_and_drops_the_reason_and_agent() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());

        assert!(on_user_prompt(&project, "login", "use postgres").unwrap());

        let state = state(&project);
        assert_eq!(state.progress, Progress::Wip);
        assert_eq!(state.blocked_reason, None);
        assert_eq!(state.blocked_by, None);
    }

    #[test]
    fn the_spawn_prompt_leaves_the_feature_blocked() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());

        assert!(!on_user_prompt(&project, "login", SPAWN_PROMPT).unwrap());

        let state = state(&project);
        assert_eq!(state.progress, Progress::Blocked);
        assert_eq!(state.blocked_reason.as_deref(), Some("which DB?"));
    }

    #[test]
    fn a_re_arm_prompt_leaves_the_feature_blocked() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        crate::messages::send(
            &paths::messages_dir(&project),
            "login",
            "implementer",
            "reviewer",
            "hi",
        )
        .unwrap();
        let rearm = hooks_stop::continuation(&project, "login", "implementer").unwrap();

        assert!(!on_user_prompt(&project, "login", &rearm).unwrap());
        assert_eq!(state(&project).progress, Progress::Blocked);
        // The user quoting it in a longer prompt is still the user.
        let quoted = format!("why did you get \"{rearm}\"?");
        assert!(on_user_prompt(&project, "login", &quoted).unwrap());
    }

    #[test]
    fn a_feature_that_is_not_blocked_is_left_alone() {
        let dir = tempdir().unwrap();
        let (project, _) =
            TestServer::new().setup_project_with_feature_no_tmux(dir.path(), "login");
        std::fs::write(
            crate::commands::feat_summary::path(&project, "login").unwrap(),
            "notes",
        )
        .unwrap();
        feat_status(&project, "login", Progress::Ready, None, None).unwrap();

        assert!(!on_user_prompt(&project, "login", "one more thing").unwrap());
        assert_eq!(state(&project).progress, Progress::Ready);
    }

    #[test]
    fn main_scope_does_nothing() {
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());

        assert!(!on_user_prompt(&project, "main", "use postgres").unwrap());
        assert_eq!(state(&project).progress, Progress::Blocked);
    }

    #[test]
    fn any_prompt_clears_the_agents_marker_in_main_too() {
        use crate::state::runtime::{Waiting, WaitingKind};
        let dir = tempdir().unwrap();
        let project = blocked_feature(dir.path());
        let interrupted = Waiting::now(WaitingKind::Interrupted, None);

        runtime::write_waiting(&project, "main", "main", &interrupted).unwrap();
        assert!(on_prompt(&project, "main", "main", "go on").unwrap());
        assert_eq!(runtime::read_waiting(&project, "main", "main"), None);
        assert!(!on_prompt(&project, "main", "main", "go on").unwrap());

        runtime::write_waiting(&project, "login", "implementer", &interrupted).unwrap();
        assert!(on_prompt(&project, "login", "implementer", SPAWN_PROMPT).unwrap());
        assert_eq!(
            runtime::read_waiting(&project, "login", "implementer"),
            None
        );
        assert_eq!(state(&project).progress, Progress::Blocked);
    }
}
