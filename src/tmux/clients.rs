//! The clients attached to a server. A bare `switch-client` moves whichever
//! client tmux takes to be current — not necessarily one viewing the session
//! about to go, and from outside tmux any client at all — so pm only ever
//! moves a client it names with `-c`, and only one viewing a session it is
//! about to kill.

use super::{no_server, run_tmux};
use crate::error::{PmError, Result};

/// An attached client and the sessions it is and was viewing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub name: String,
    pub session: String,
    /// The session it viewed before this one, if any.
    pub last_session: Option<String>,
}

/// Every client attached to the server; none when no server runs.
pub fn list(server: Option<&str>) -> Result<Vec<Client>> {
    let output = match run_tmux(
        server,
        &[
            "list-clients",
            "-F",
            "#{client_name}\t#{client_session}\t#{client_last_session}",
        ],
    ) {
        Ok(output) => output,
        Err(PmError::Tmux(msg)) if no_server(&msg) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.to_string();
            let session = fields.next()?.to_string();
            let last_session = fields.next().filter(|s| !s.is_empty()).map(String::from);
            Some(Client {
                name,
                session,
                last_session,
            })
        })
        .collect())
}

/// Move each client viewing one of `doomed` to `preferred`, else to the
/// session it viewed last, else to any other session; one with nowhere left
/// to go is left for tmux to detach when its session is killed. Clients
/// viewing anything else stay where they are.
pub fn move_off(server: Option<&str>, doomed: &[String], preferred: Option<&str>) -> Result<()> {
    let clients: Vec<Client> = list(server)?
        .into_iter()
        .filter(|c| doomed.contains(&c.session))
        .collect();
    if clients.is_empty() {
        return Ok(());
    }
    let survivors: Vec<String> = super::list_sessions(server)?
        .into_iter()
        .filter(|s| !doomed.contains(s))
        .collect();
    let survives = |s: &&str| survivors.iter().any(|v| v == s);
    for client in clients {
        let destination = preferred
            .filter(survives)
            .or(client.last_session.as_deref().filter(survives))
            .or(survivors.first().map(String::as_str));
        if let Some(destination) = destination {
            // A client that detached since it was listed has nothing to move.
            let _ = super::switch_client_of(server, &client.name, destination);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ControlClient, OwnServer};
    use crate::tmux::create_session;
    use tempfile::tempdir;

    fn viewing(server: &OwnServer) -> Vec<String> {
        list(server.name())
            .unwrap()
            .into_iter()
            .map(|c| c.session)
            .collect()
    }

    fn sessions(server: &OwnServer, dir: &std::path::Path, names: &[&str]) {
        for name in names {
            create_session(server.name(), name, dir).unwrap();
        }
    }

    #[test]
    fn only_clients_viewing_a_doomed_session_move_and_to_the_preferred_one() {
        let server = OwnServer::start("clients-preferred");
        let dir = tempdir().unwrap();
        sessions(&server, dir.path(), &["p/main", "p/api", "elsewhere"]);
        let _on_api = ControlClient::attach(server.name(), "p/api");
        let _elsewhere = ControlClient::attach(server.name(), "elsewhere");

        move_off(server.name(), &["p/api".into()], Some("p/main")).unwrap();

        let mut seen = viewing(&server);
        seen.sort();
        assert_eq!(seen, ["elsewhere", "p/main"]);
    }

    #[test]
    fn a_client_without_a_surviving_preference_returns_to_its_last_session() {
        let server = OwnServer::start("clients-last");
        let dir = tempdir().unwrap();
        sessions(&server, dir.path(), &["a", "b", "p/main", "p/api"]);
        let mut client = ControlClient::attach(server.name(), "b");
        let name = list(server.name()).unwrap()[0].name.clone();
        crate::tmux::switch_client_of(server.name(), &name, "p/api").unwrap();
        client.sync();

        let doomed = ["p/main".to_string(), "p/api".to_string()];
        move_off(server.name(), &doomed, Some("p/main")).unwrap();

        assert_eq!(viewing(&server), ["b"]);
    }

    #[test]
    fn a_client_whose_last_session_is_also_doomed_goes_to_a_survivor() {
        let server = OwnServer::start("clients-survivor");
        let dir = tempdir().unwrap();
        sessions(&server, dir.path(), &["p/main", "p/api"]);
        let mut client = ControlClient::attach(server.name(), "p/main");
        let name = list(server.name()).unwrap()[0].name.clone();
        crate::tmux::switch_client_of(server.name(), &name, "p/api").unwrap();
        client.sync();

        let doomed = ["p/main".to_string(), "p/api".to_string()];
        move_off(server.name(), &doomed, None).unwrap();

        assert_eq!(viewing(&server), ["keepalive"]);
    }
}
