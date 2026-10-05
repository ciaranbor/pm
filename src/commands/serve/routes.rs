//! What each request gets. The token is checked before anything else, so
//! a request without a valid one learns nothing, not even which paths
//! exist; a paired device's token may do everything. The writes are a
//! device's own push subscription and an agent's input (`input`).
//! Path segments name only what the registry and pm state list, so none
//! reaches the filesystem as a path of its own.

use std::path::PathBuf;

use crate::commands::{attention, feat_info};
use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::devices::{Devices, Push};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectEntry;
use crate::tmux;

use super::transcript::{Agent, DEFAULT_LIMIT, MAX_LIMIT, TranscriptWatch, page_json};
use super::{Config, input, push};

pub(super) enum Reply {
    Body {
        status: u16,
        content_type: &'static str,
        body: String,
    },
    /// Hand the connection to an event stream, watching an agent's
    /// conversation if the request named one.
    Events(Option<Box<TranscriptWatch>>),
}

impl Reply {
    pub(super) fn status(&self) -> u16 {
        match self {
            Self::Body { status, .. } => *status,
            Self::Events(_) => 200,
        }
    }
}

pub(super) struct Handled {
    /// The device whose token the request carried.
    pub device: Option<String>,
    pub reply: Reply,
    /// What a write did, for the request log.
    pub detail: Option<String>,
}

const JSON: &str = "application/json";
const MARKDOWN: &str = "text/markdown; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

pub(super) fn error(status: u16, message: &str) -> Reply {
    Reply::Body {
        status,
        content_type: JSON,
        body: serde_json::json!({ "error": message }).to_string(),
    }
}

pub(super) fn json(status: u16, body: serde_json::Value) -> Reply {
    Reply::Body {
        status,
        content_type: JSON,
        body: body.to_string(),
    }
}

fn ok(content_type: &'static str, body: String) -> Reply {
    Reply::Body {
        status: 200,
        content_type,
        body,
    }
}

/// One request, as `route` reads it.
pub(super) struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub authorization: Option<&'a str>,
    pub body: &'a str,
}

/// Answer `request`; `vapid` is the public key the server signs pushes
/// with.
pub(super) fn route(config: &Config, vapid: &str, request: &Request<'_>) -> Handled {
    let Request {
        method,
        path,
        query,
        authorization,
        body,
    } = *request;
    let token = authorization.and_then(bearer);
    let devices = match Devices::load(&config.devices) {
        Ok(devices) => devices,
        Err(e) => {
            return Handled {
                device: None,
                reply: error(500, &format!("devices unreadable: {e}")),
                detail: None,
            };
        }
    };
    let Some((device, _)) = token.and_then(|t| devices.authenticate(t.trim())) else {
        return Handled {
            device: None,
            reply: error(401, "a paired device's bearer token is required"),
            detail: None,
        };
    };
    let mut detail = None;
    let served = match (method, path) {
        (_, "/v1/push") => push_route(config, vapid, method, device, body),
        ("GET", _) => get(config, path, &Query::parse(query)),
        ("POST", _) if path.starts_with("/v1/agents/") => post(config, path, body).map(|written| {
            detail = Some(written.detail).filter(|d| !d.is_empty());
            written.reply
        }),
        _ => Ok(error(405, "only GET is served here")),
    };
    let reply = served.unwrap_or_else(|e| error(500, &e.to_string()));
    Handled {
        device: Some(device.to_string()),
        reply,
        detail,
    }
}

/// `POST /v1/agents/{project}/{scope}/{agent}/{action}`: input for an agent.
fn post(config: &Config, path: &str, body: &str) -> Result<input::Written> {
    let segments = segments(path);
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let ["agents", project, scope, agent, action] = segments[..] else {
        return Ok(input::Written {
            reply: error(404, "no such endpoint"),
            detail: String::new(),
        });
    };
    match find_agent(config, project, scope, agent)? {
        Ok(agent) => input::post(&agent, action, body, config.tmux_server.as_deref()),
        Err(reply) => Ok(input::Written {
            reply,
            detail: String::new(),
        }),
    }
}

/// `path`'s segments after `/v1/`, decoded; none for another path.
fn segments(path: &str) -> Vec<String> {
    path.strip_prefix("/v1/")
        .map(|rest| rest.split('/').map(decode).collect())
        .unwrap_or_default()
}

/// `/v1/push`: the server's VAPID public key, and the device's own
/// subscription to set or clear.
fn push_route(
    config: &Config,
    vapid: &str,
    method: &str,
    device: &str,
    body: &str,
) -> Result<Reply> {
    let set = |push: Option<Push>| {
        Devices::update(&config.devices, |paired| {
            if let Some(d) = paired.devices.get_mut(device) {
                d.push = push;
            }
            Ok(())
        })
    };
    match method {
        "GET" => {
            let body = serde_json::json!({ "vapid": vapid });
            Ok(ok(JSON, body.to_string()))
        }
        "PUT" => match push::subscription(body, &config.push) {
            Ok(push) => {
                set(Some(push))?;
                Ok(no_content())
            }
            Err(e) => Ok(error(400, &e)),
        },
        "DELETE" => {
            set(None)?;
            Ok(no_content())
        }
        _ => Ok(error(405, "GET, PUT and DELETE are served here")),
    }
}

fn no_content() -> Reply {
    Reply::Body {
        status: 204,
        content_type: JSON,
        body: String::new(),
    }
}

/// The token of a `Bearer` credential; the scheme is case-insensitive.
fn bearer(authorization: &str) -> Option<&str> {
    let (scheme, token) = authorization.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token)
}

/// A request's query parameters, decoded.
struct Query(Vec<(String, String)>);

impl Query {
    fn parse(query: &str) -> Self {
        Self(
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                    (decode(key), decode(value))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn get(config: &Config, path: &str, query: &Query) -> Result<Reply> {
    let segments = segments(path);
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let server = config.tmux_server.as_deref();
    match segments[..] {
        ["snapshot"] => {
            let snapshot = attention::all(&config.projects_dir, server)?;
            Ok(ok(JSON, serde_json::to_string(&snapshot)?))
        }
        ["events"] => {
            let Some(watch) = query.get("watch") else {
                return Ok(Reply::Events(None));
            };
            let [project, scope, agent] = watch.split('/').collect::<Vec<_>>()[..] else {
                return Ok(error(400, "watch names <project>/<scope>/<agent>"));
            };
            Ok(match find_agent(config, project, scope, agent)? {
                Ok(agent) => Reply::Events(Some(Box::new(TranscriptWatch::new(
                    agent,
                    query.get("after").map(str::to_string),
                )))),
                Err(reply) => reply,
            })
        }
        ["agents", project, scope, agent, "transcript"] => {
            let agent = match find_agent(config, project, scope, agent)? {
                Ok(agent) => agent,
                Err(reply) => return Ok(reply),
            };
            let limit = match query.get("limit").map(str::parse::<usize>) {
                None => DEFAULT_LIMIT,
                Some(Ok(limit)) if (1..=MAX_LIMIT).contains(&limit) => limit,
                Some(_) => {
                    return Ok(error(
                        400,
                        &format!("limit is a number from 1 to {MAX_LIMIT}"),
                    ));
                }
            };
            let Some(conversation) = agent.conversation()? else {
                return Ok(error(404, "the agent has no conversation yet"));
            };
            let page = conversation.page(query.get("before"), limit)?;
            Ok(ok(JSON, page_json(&conversation, &page)?))
        }
        ["agents", project, scope, agent, "transcript", "result"] => {
            let agent = match find_agent(config, project, scope, agent)? {
                Ok(agent) => agent,
                Err(reply) => return Ok(reply),
            };
            let Some(reference) = query.get("ref") else {
                return Ok(error(400, "ref names the result"));
            };
            let Some(conversation) = agent.conversation()? else {
                return Ok(error(404, "the agent has no conversation yet"));
            };
            match conversation.full_result(reference)? {
                Some(text) => Ok(ok(TEXT, text)),
                None => Ok(error(404, "no such result")),
            }
        }
        ["features", project, feature] => {
            let Some(root) = project_root(config, project)? else {
                return Ok(error(404, "no such project"));
            };
            if !has_feature(&root, feature)? {
                return Ok(error(404, "no such feature"));
            }
            let info = feat_info::info(&root, &config.projects_dir, feature)?;
            Ok(ok(JSON, serde_json::to_string(&info)?))
        }
        ["features", project, feature, "summary"] => {
            let Some(root) = project_root(config, project)? else {
                return Ok(error(404, "no such project"));
            };
            if !has_feature(&root, feature)? {
                return Ok(error(404, "no such feature"));
            }
            match std::fs::read_to_string(paths::summary_path(&root, feature)) {
                Ok(summary) => Ok(ok(MARKDOWN, summary)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok(error(404, "the feature has no summary"))
                }
                Err(e) => Err(e.into()),
            }
        }
        ["agents", project, scope, agent, "screen"] => {
            let Some(root) = project_root(config, project)? else {
                return Ok(error(404, "no such project"));
            };
            if scope != "main" && !has_feature(&root, scope)? {
                return Ok(error(404, "no such scope"));
            }
            let agents = attention::scope_agents(&root, scope, server)?;
            let Some(found) = agents.iter().find(|a| a.name == agent) else {
                return Ok(error(404, "no such agent"));
            };
            let Some(pane) = &found.pane else {
                return Ok(error(404, "the agent has no window"));
            };
            Ok(ok(TEXT, tmux::capture_visible(server, pane)?))
        }
        _ => Ok(error(404, "no such endpoint")),
    }
}

/// The registered agent `name` of `scope` in `project`, or the reply saying
/// which of them does not exist.
fn find_agent(
    config: &Config,
    project: &str,
    scope: &str,
    name: &str,
) -> Result<std::result::Result<Agent, Reply>> {
    let Some(root) = project_root(config, project)? else {
        return Ok(Err(error(404, "no such project")));
    };
    if scope != "main" && !has_feature(&root, scope)? {
        return Ok(Err(error(404, "no such scope")));
    }
    let registry = AgentRegistry::load(&paths::agents_dir(&root), scope)?;
    if registry.get(name).is_none() {
        return Ok(Err(error(404, "no such agent")));
    }
    Ok(Ok(Agent {
        project: project.to_string(),
        scope: scope.to_string(),
        name: name.to_string(),
        root,
    }))
}

/// The root of the registered project named `name`.
fn project_root(config: &Config, name: &str) -> Result<Option<PathBuf>> {
    Ok(ProjectEntry::scan(&config.projects_dir)?
        .projects
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, entry)| entry.root_path()))
}

fn has_feature(root: &std::path::Path, name: &str) -> Result<bool> {
    Ok(FeatureState::list(&paths::features_dir(root))?
        .iter()
        .any(|(n, _)| n == name))
}

/// A path segment with its `%XX` escapes decoded; one that doesn't decode
/// to UTF-8 is kept as it came, and matches nothing.
fn decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| segment.to_string())
}
