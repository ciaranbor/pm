//! A client of an agent's own `codex app-server`: JSON-RPC text frames
//! over a WebSocket on the unix socket the server listens on (`--listen
//! unix://<path>`), `initialize` and `initialized` first, then requests
//! matched to their responses by id among the notifications the server
//! sends freely.
//!
//! codex binds the real socket under `/tmp/codex-daemon-<uid>/` and makes
//! the requested path a symlink to it, so a path too long for `sun_path`
//! still works for codex; pm connects to the symlink's target for the same
//! reason.
//!
//! The client opts into the experimental API: `turn/start`'s
//! `permissions` (a named profile) is gated behind it.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::error::{PmError, Result};

/// A connection to an app-server.
pub(in crate::harness) struct Client {
    socket: PathBuf,
    ws: WebSocket<UnixStream>,
    next_id: u64,
    limit: Duration,
    /// When every exchange of this connection must be over, if ever.
    end: Option<Instant>,
}

/// Where `socket` really is: the target of the symlink codex makes it.
fn resolve(socket: &Path) -> PathBuf {
    match std::fs::read_link(socket) {
        Ok(target) if target.is_relative() => socket
            .parent()
            .map_or(target.clone(), |dir| dir.join(&target)),
        Ok(target) => target,
        Err(_) => socket.to_path_buf(),
    }
}

impl Client {
    /// Connect to the app-server listening on `socket` and initialize,
    /// giving each exchange up to `limit`.
    pub(in crate::harness) fn connect(socket: &Path, limit: Duration) -> Result<Self> {
        Self::open(socket, limit, None)
    }

    /// [`connect`](Self::connect), with everything this connection does,
    /// initializing included, over within `budget`.
    pub(in crate::harness) fn within(socket: &Path, budget: Duration) -> Result<Self> {
        Self::open(socket, budget, Some(Instant::now() + budget))
    }

    fn open(socket: &Path, limit: Duration, end: Option<Instant>) -> Result<Self> {
        let gone = |e: &dyn std::fmt::Display| {
            PmError::Agent(format!(
                "its codex app-server at {} is gone ({e}): the session that ran it has ended",
                socket.display()
            ))
        };
        let stream = UnixStream::connect(resolve(socket)).map_err(|e| gone(&e))?;
        stream.set_read_timeout(Some(limit))?;
        stream.set_write_timeout(Some(limit))?;
        let (ws, _) = tungstenite::client("ws://localhost/", stream).map_err(|e| gone(&e))?;
        let mut client = Self {
            socket: socket.to_path_buf(),
            ws,
            next_id: 0,
            limit,
            end,
        };
        client.call(
            "initialize",
            json!({
                "clientInfo": {"name": "pm", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true},
            }),
        )?;
        client.send(json!({"method": "initialized"}))?;
        Ok(client)
    }

    fn send(&mut self, message: Value) -> Result<()> {
        self.ws
            .send(Message::text(message.to_string()))
            .map_err(|e| self.error(&e))
    }

    fn error(&self, e: &dyn std::fmt::Display) -> PmError {
        PmError::Agent(format!(
            "codex app-server at {}: {e}",
            self.socket.display()
        ))
    }

    /// Send request `method` with `params` and return its result.
    pub(in crate::harness) fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id": id, "method": method, "params": params}))?;
        let deadline = self.end.map_or(Instant::now() + self.limit, |end| {
            end.min(Instant::now() + self.limit)
        });
        while let Some(left) = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        {
            self.ws.get_ref().set_read_timeout(Some(left))?;
            let message = self.ws.read().map_err(|e| self.error(&e))?;
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(reply) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if reply.get("method").is_some() || reply.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| error.to_string(), str::to_string);
                return Err(self.error(&format!("`{method}` failed: {message}")));
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(self.error(&format!("`{method}` got no answer")))
    }

    /// The threads the server holds in memory.
    pub(in crate::harness) fn loaded_threads(&mut self) -> Result<Vec<String>> {
        let result = self.call("thread/loaded/list", json!({}))?;
        Ok(result
            .get("data")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Start a turn of thread `thread` with `text` as the user's input, or
    /// steer it into the turn running; `permissions` (fields of
    /// `turn/start`) apply to it and every later turn.
    pub(in crate::harness) fn start_turn(
        &mut self,
        thread: &str,
        text: &str,
        permissions: Option<&Permissions>,
    ) -> Result<()> {
        let mut params = json!({
            "threadId": thread,
            "input": [{"type": "text", "text": text}],
        });
        if let (Some(permissions), Value::Object(fields)) = (permissions, &mut params) {
            fields.extend(permissions.fields());
        }
        self.call("turn/start", params).map(|_| ())
    }
}

/// The approval policy and sandbox an agent's turns run under, as pm
/// config resolved them at its spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::harness) struct Permissions {
    pub approval: String,
    pub sandbox: Sandbox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::harness) enum Sandbox {
    /// A named permission profile the server defines.
    Profile(String),
    WorkspaceWrite(Vec<PathBuf>),
    FullAccess,
    /// Another sandbox mode, passed through for codex to judge.
    Other(String),
}

impl Permissions {
    fn fields(&self) -> serde_json::Map<String, Value> {
        let mut fields = serde_json::Map::new();
        fields.insert("approvalPolicy".into(), json!(self.approval));
        let (key, value) = match &self.sandbox {
            Sandbox::Profile(name) => ("permissions", json!(name)),
            Sandbox::WorkspaceWrite(roots) => (
                "sandboxPolicy",
                json!({"type": "workspaceWrite", "writableRoots": roots}),
            ),
            Sandbox::FullAccess => ("sandboxPolicy", json!({"type": "dangerFullAccess"})),
            Sandbox::Other(mode) => ("sandboxPolicy", json!({"type": mode})),
        };
        fields.insert(key.into(), value);
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeAppServer;

    const LIMIT: Duration = Duration::from_secs(5);

    #[test]
    fn a_turn_is_started_after_initializing_with_the_experimental_api() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeAppServer::bind(dir.path());
        let mut client = Client::connect(&server.path, LIMIT).unwrap();
        let permissions = Permissions {
            approval: "never".into(),
            sandbox: Sandbox::WorkspaceWrite(vec![PathBuf::from("/p/.pm")]),
        };
        client
            .start_turn("t1", "read your messages", Some(&permissions))
            .unwrap();

        let init = server.request();
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
        assert_eq!(server.request()["method"], "initialized");
        let turn = server.request();
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(
            turn["params"],
            json!({
                "threadId": "t1",
                "input": [{"type": "text", "text": "read your messages"}],
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "workspaceWrite", "writableRoots": ["/p/.pm"]},
            })
        );
    }

    #[test]
    fn each_sandbox_is_sent_in_the_field_codex_reads_it_from() {
        let fields = |sandbox| {
            Value::Object(
                Permissions {
                    approval: "on-request".into(),
                    sandbox,
                }
                .fields(),
            )
        };
        assert_eq!(
            fields(Sandbox::Profile("pm-read-only".into())),
            json!({"approvalPolicy": "on-request", "permissions": "pm-read-only"})
        );
        assert_eq!(
            fields(Sandbox::FullAccess),
            json!({"approvalPolicy": "on-request", "sandboxPolicy": {"type": "dangerFullAccess"}})
        );
    }

    #[test]
    fn a_long_socket_path_is_reached_through_its_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeAppServer::bind(dir.path());
        let deep = dir.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let link = deep.join("app-server.sock");
        std::os::unix::fs::symlink(&server.path, &link).unwrap();
        assert!(link.as_os_str().len() > 104);

        let mut client = Client::connect(&link, LIMIT).unwrap();
        assert_eq!(client.loaded_threads().unwrap(), ["t1"]);
    }

    #[test]
    fn a_server_error_and_a_dead_socket_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let server = FakeAppServer::bind(dir.path());
        let mut client = Client::connect(&server.path, LIMIT).unwrap();
        let err = client
            .start_turn("missing", "hi", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`turn/start` failed: no thread missing"),
            "{err}"
        );

        drop(server);
        let err = Client::connect(&dir.path().join("gone.sock"), LIMIT)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("is gone"), "{err}");
    }
}
