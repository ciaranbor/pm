//! A stand-in for an agent's opencode server: it answers every request with
//! one status and body, and hands each request back.

use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

/// An HTTP server on a loopback port, serving until the test ends.
pub struct FakeOpenCodeServer {
    pub url: String,
    requests: mpsc::Receiver<OpenCodeRequest>,
}

/// A request a [`FakeOpenCodeServer`] received.
#[derive(Debug)]
pub struct OpenCodeRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: Value,
}

impl FakeOpenCodeServer {
    /// A server answering every request with `status` and `body`.
    pub fn answering(status: u16, body: &'static str) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let (sender, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut text = String::new();
                let _ = request.as_reader().read_to_string(&mut text);
                let authorization = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.to_string());
                let received = OpenCodeRequest {
                    method: request.method().to_string(),
                    path: request.url().to_string(),
                    authorization,
                    body: serde_json::from_str(&text).unwrap_or(Value::Null),
                };
                let response = tiny_http::Response::from_string(body).with_status_code(status);
                let _ = request.respond(response);
                if sender.send(received).is_err() {
                    return;
                }
            }
        });
        Self { url, requests }
    }

    /// The next request a client sent; `None` when none came within a
    /// moment.
    pub fn request(&self) -> Option<OpenCodeRequest> {
        self.requests.recv_timeout(Duration::from_secs(2)).ok()
    }
}
