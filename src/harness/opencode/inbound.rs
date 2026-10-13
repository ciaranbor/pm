//! An opencode session's inbound channel: its agent's server
//! ([`host`](super::host)), prompted over HTTP as the TUI is.
//!
//! A post is `session.prompt`, which starts a turn on an idle or
//! interrupted session and steers into a running one, delivered at its next
//! step. It carries the plugin's own `metadata`, so the plugin does not
//! report it as the user's prompt.

use std::path::PathBuf;
use std::time::Duration;

use base64ct::{Base64, Encoding};
use serde_json::{Value, json};

use super::SERVER_FILE_ENV;
use super::host::read_address;
use crate::error::{PmError, Result};
use crate::state::runtime::{Endpoint, Inbound};

/// opencode's basic-auth user for a server's password.
const USER: &str = "opencode";
/// How long a post may take, connecting included.
const POST_WITHIN: Duration = Duration::from_secs(3);

/// The inbound channel of session `session`, as its SessionStart hook,
/// run by the plugin in the agent's server, finds it in its environment:
/// the file the host keeps the server's address in. A session launched
/// without the host has none.
pub(in crate::harness) fn inbound(session: &str) -> Option<Inbound> {
    from_server_file(
        std::env::var_os(SERVER_FILE_ENV).map(PathBuf::from),
        session,
    )
}

fn from_server_file(file: Option<PathBuf>, session: &str) -> Option<Inbound> {
    Some(Inbound {
        endpoint: Endpoint::ServerFile(file.filter(|f| !f.as_os_str().is_empty())?),
        token: None,
        thread: Some(session.to_string()).filter(|s| !s.is_empty()),
    })
}

/// Prompt the session on `inbound` with `text`.
pub(in crate::harness) fn post(inbound: &Inbound, text: &str) -> Result<()> {
    let (Some(file), Some(session)) = (inbound.server_file(), inbound.thread.as_deref()) else {
        return Err(PmError::Agent(
            "its opencode session reported no server and session to prompt".to_string(),
        ));
    };
    let address = read_address(file).ok_or_else(|| {
        PmError::Agent(format!(
            "its opencode server's address {} is unreadable: its host never started the server",
            file.display()
        ))
    })?;
    let url = &address.url;
    let credentials = Base64::encode_string(format!("{USER}:{}", address.password).as_bytes());
    let body = json!({"text": text, "metadata": {"pm": "continuation"}}).to_string();
    let mut response = agent()
        .post(format!("{url}/api/session/{session}/prompt"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Basic {credentials}"))
        .send(body)
        .map_err(|e| {
            PmError::Agent(format!(
                "could not reach its opencode server at {url} ({e}): the agent's window has \
                 ended"
            ))
        })?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let answer = response.body_mut().read_to_string().unwrap_or_default();
    let tag = serde_json::from_str::<Value>(&answer)
        .ok()
        .and_then(|error| Some(error.get("_tag")?.as_str()?.to_string()))
        .unwrap_or_else(|| answer.trim().to_string());
    let why = format!("{status} {tag}");
    Err(PmError::Agent(format!(
        "its opencode server at {url} refused the prompt to session {session}: {}",
        why.trim_end()
    )))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(POST_WITHIN))
        .http_status_as_error(false)
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::opencode::host::{ServerAddress, write_address};
    use crate::testing::FakeOpenCodeServer;

    /// The inbound channel of session `ses_1` whose server's address, `url`
    /// with password `pw`, is in a file in `dir`.
    fn inbound(dir: &std::path::Path, url: &str) -> Inbound {
        let file = dir.join("server.json");
        let address = ServerAddress {
            url: url.to_string(),
            password: "pw".to_string(),
        };
        write_address(&file, &address).unwrap();
        from_server_file(Some(file), "ses_1").unwrap()
    }

    #[test]
    fn a_post_prompts_the_session_as_pm_with_the_servers_password() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeOpenCodeServer::answering(200, r#"{"data":{"delivery":"steer"}}"#);
        post(&inbound(dir.path(), &server.url), "Check your messages.").unwrap();
        let request = server.request().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/session/ses_1/prompt");
        assert_eq!(
            request.authorization.as_deref(),
            Some(format!("Basic {}", Base64::encode_string(b"opencode:pw")).as_str())
        );
        assert_eq!(
            request.body,
            json!({"text": "Check your messages.", "metadata": {"pm": "continuation"}})
        );
    }

    #[test]
    fn a_refused_or_unreachable_post_is_an_error_naming_why() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeOpenCodeServer::answering(401, r#"{"_tag":"UnauthorizedError"}"#);
        let err = post(&inbound(dir.path(), &server.url), "x")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("401") && err.contains("UnauthorizedError"),
            "{err}"
        );

        let server = FakeOpenCodeServer::answering(404, r#"{"_tag":"SessionNotFoundError"}"#);
        let err = post(&inbound(dir.path(), &server.url), "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("SessionNotFoundError"), "{err}");

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let err = post(&inbound(dir.path(), &url), "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("could not reach"), "{err}");

        let never_written = from_server_file(Some(dir.path().join("none.json")), "ses_1");
        let err = post(&never_written.unwrap(), "x").unwrap_err().to_string();
        assert!(err.contains("none.json is unreadable"), "{err}");
    }

    #[test]
    fn a_session_without_a_host_has_no_inbound_channel() {
        assert_eq!(from_server_file(None, "ses_1"), None);
        assert_eq!(from_server_file(Some(PathBuf::new()), "ses_1"), None);
    }
}
