//! A stand-in for an agent's `codex app-server`, recording the JSON-RPC
//! messages its clients send. It holds one thread, `t1`, sends a
//! notification ahead of every answer, and refuses a turn of thread
//! `missing`.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::Message;

/// A WebSocket server on a unix socket in `dir`, serving until dropped.
pub struct FakeAppServer {
    pub path: PathBuf,
    requests: mpsc::Receiver<Value>,
}

impl FakeAppServer {
    pub fn bind(dir: &Path) -> Self {
        let path = dir.join("app.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let Ok(mut ws) = tungstenite::accept(stream) else {
                        return;
                    };
                    while let Ok(Message::Text(text)) = ws.read() {
                        let Ok(request) = serde_json::from_str::<Value>(text.as_str()) else {
                            continue;
                        };
                        let id = request.get("id").cloned();
                        let reply = answer(&request);
                        if tx.send(request).is_err() {
                            return;
                        }
                        let Some(id) = id else { continue };
                        let mut reply = reply;
                        reply["id"] = id;
                        let notice = json!({"method": "thread/status/changed", "params": {}});
                        if ws.send(Message::text(notice.to_string())).is_err()
                            || ws.send(Message::text(reply.to_string())).is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        Self { path, requests }
    }

    /// The next message a client sent.
    pub fn request(&self) -> Value {
        self.requests.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    /// The params of the next `turn/start` a client sent, skipping the
    /// rest; `None` when none came within a moment.
    pub fn turn(&self) -> Option<Value> {
        while let Ok(request) = self.requests.recv_timeout(Duration::from_millis(500)) {
            if request["method"] == "turn/start" {
                return Some(request["params"].clone());
            }
        }
        None
    }
}

fn answer(request: &Value) -> Value {
    let params = &request["params"];
    match request["method"].as_str().unwrap_or_default() {
        "thread/loaded/list" => json!({"result": {"data": ["t1"]}}),
        "turn/start" if params["threadId"] == "missing" => {
            json!({"error": {"code": -32600, "message": "no thread missing"}})
        }
        "thread/name/set" if params["name"] == "refused" => {
            json!({"error": {"code": -32601, "message": "method not found"}})
        }
        _ => json!({"result": {}}),
    }
}
