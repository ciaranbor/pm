//! `pm tmux jump`: what choosing an item in pm's tree does. A window or pane
//! is switched to as chosen; a feature's session goes to the window of the
//! agent its attention names — the one that set `blocked`, or a dead agent
//! whose window is still open — and otherwise to the session.

use std::path::Path;

use crate::error::Result;
use crate::tmux;

use super::attention::{self, Snapshot};

/// Switch `client` to where choosing `target` (a tree item's target, as
/// `choose-tree` gives `%%`) should take it.
pub fn jump(
    projects_dir: &Path,
    tmux_server: Option<&str>,
    client: &str,
    target: &str,
) -> Result<()> {
    // Pm state that can't be read still leaves the chosen item to go to.
    let destination = attention::all(projects_dir, tmux_server)
        .map_or_else(|_| target.to_string(), |s| destination(&s, target));
    tmux::switch_client_of(tmux_server, client, &destination)
}

fn destination(snapshot: &Snapshot, target: &str) -> String {
    // A session item's target is `=name:`; a session name holds no `:`.
    let Some(session) = target
        .strip_prefix('=')
        .and_then(|t| t.strip_suffix(':'))
        .filter(|s| !s.contains(':'))
    else {
        return target.to_string();
    };
    snapshot
        .features
        .iter()
        .find(|f| f.session == session)
        .and_then(|f| {
            let agent = f.attention.agent.as_deref()?;
            f.agents.iter().find(|a| a.name == agent)?.window.clone()
        })
        .map_or_else(|| target.to_string(), |window| format!("={window}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feat_status::feat_status;
    use crate::state::feature::Progress;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    #[test]
    fn a_blocked_features_session_goes_to_the_agent_that_blocked_it() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        let reviewer = server.spawn_idle_fake_agent(&project, &session, "login", "reviewer");
        let item = format!("={session}:");
        let snapshot = || attention::all(&projects_dir, server.name()).unwrap();

        assert_eq!(destination(&snapshot(), &item), item, "nothing to go to");

        feat_status(
            &project,
            "login",
            Progress::Blocked,
            Some("which DB?"),
            Some("reviewer"),
        )
        .unwrap();
        assert_eq!(destination(&snapshot(), &item), format!("={reviewer}"));

        let window = format!("={session}:0.");
        assert_eq!(
            destination(&snapshot(), &window),
            window,
            "a window as chosen"
        );
    }

    #[test]
    fn a_session_goes_to_a_dead_agents_open_window_and_main_as_chosen() {
        let dir = tempdir().unwrap();
        let server = TestServer::new();
        let (project, project_name) = server.setup_project_with_feature(dir.path(), "login");
        let projects_dir = TestServer::registry_dir(&project);
        let session = tmux::session_name(&project_name, "login");
        server.spawn_idle_fake_agent(&project, &session, "login", "implementer");
        let reviewer = server.spawn_dead_fake_agent(&project, &session, "login", "reviewer");
        let snapshot = attention::all(&projects_dir, server.name()).unwrap();

        assert_eq!(
            destination(&snapshot, &format!("={session}:")),
            format!("={reviewer}")
        );
        let main = format!("={}:", tmux::session_name(&project_name, "main"));
        assert_eq!(destination(&snapshot, &main), main);
    }
}
