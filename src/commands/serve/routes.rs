//! What each request gets. The token is checked before anything else, so
//! a request without a valid one learns nothing, not even which paths
//! exist; a paired device's token may do everything.
//! Path segments name only what the registry and pm state list, so none
//! reaches the filesystem as a path of its own.

use std::path::PathBuf;

use crate::commands::{attention, feat_info, feat_merge};
use crate::error::Result;
use crate::state::agent::AgentRegistry;
use crate::state::devices::{Devices, Push};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectEntry;
use crate::tmux;

use super::transcript::{Agent, DEFAULT_LIMIT, MAX_LIMIT, TranscriptWatch, page_json};
use super::{Config, input, lifecycle, notes, push};

pub(super) enum Reply {
    Body {
        status: u16,
        content_type: &'static str,
        body: String,
        /// The version the body is, sent as its `ETag`.
        etag: Option<String>,
    },
    /// Hand the connection to an event stream for the token whose SHA-256
    /// is `token_sha256`, watching an agent's conversation if the request
    /// named one.
    Events {
        token_sha256: String,
        watch: Option<Box<TranscriptWatch>>,
    },
}

impl Reply {
    pub(super) fn status(&self) -> u16 {
        match self {
            Self::Body { status, .. } => *status,
            Self::Events { .. } => 200,
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

pub(super) const JSON: &str = "application/json";
pub(super) const MARKDOWN: &str = "text/markdown; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

/// Paths a released app may still ask for that this server no longer
/// serves. See docs/remote-api.md.
const RETIRED: &[&str] = &["/v1/device"];

pub(super) fn error(status: u16, message: &str) -> Reply {
    Reply::Body {
        status,
        content_type: JSON,
        body: serde_json::json!({ "error": message }).to_string(),
        etag: None,
    }
}

pub(super) fn json(status: u16, body: serde_json::Value) -> Reply {
    Reply::Body {
        status,
        content_type: JSON,
        body: body.to_string(),
        etag: None,
    }
}

fn ok(content_type: &'static str, body: String) -> Reply {
    Reply::Body {
        status: 200,
        content_type,
        body,
        etag: None,
    }
}

/// One request, as `route` reads it.
pub(super) struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub authorization: Option<&'a str>,
    pub if_match: Option<&'a str>,
    pub body: &'a str,
    /// The body was longer than the server reads; `body` is then empty.
    pub too_long: bool,
}

/// Answer `request`; `vapid` is the public key the server signs pushes
/// with.
pub(super) fn route(config: &Config, vapid: &str, request: &Request<'_>) -> Handled {
    let Request {
        method,
        path,
        query,
        authorization,
        if_match,
        body,
        too_long,
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
    let Some((device, paired)) = token.and_then(|t| devices.authenticate(t.trim())) else {
        return Handled {
            device: None,
            reply: error(401, "a paired device's bearer token is required"),
            detail: None,
        };
    };
    if too_long {
        let reply = if path.starts_with("/v1/projects/") && path.ends_with("/notes") {
            notes::too_long()
        } else {
            error(413, "the body is too long")
        };
        return Handled {
            device: Some(device.to_string()),
            reply,
            detail: None,
        };
    }
    let mut detail = None;
    let served = match (method, path) {
        (_, _) if RETIRED.contains(&path) => Ok(error(410, "this endpoint was retired")),
        (_, "/v1/push") => push_route(config, vapid, method, device, body),
        ("DELETE", "/v1/pairing") => {
            crate::commands::serve_revoke::revoke(&config.devices, device).map(|()| no_content())
        }
        (_, "/v1/pairing") => Ok(error(405, "DELETE is served here")),
        (_, _) if path.starts_with("/v1/projects/") => {
            projects_route(config, method, path, if_match, body).map(|(reply, written)| {
                detail = written;
                reply
            })
        }
        ("GET", _) => get(config, path, &Query::parse(query), &paired.token_sha256),
        ("POST", _) if path.starts_with("/v1/agents/") || path.starts_with("/v1/features/") => {
            post(config, path, body).map(|written| {
                detail = Some(written.detail).filter(|d| !d.is_empty());
                written.reply
            })
        }
        _ => Ok(error(405, "no such endpoint for this method")),
    };
    let reply = served.unwrap_or_else(|e| error(500, &e.to_string()));
    Handled {
        device: Some(device.to_string()),
        reply,
        detail,
    }
}

/// `POST /v1/agents/{project}/{scope}/{agent}/{action}`, input for an
/// agent or its restart, and `POST /v1/features/{project}/{feature}/{action}`,
/// a feature's merge or delete.
fn post(config: &Config, path: &str, body: &str) -> Result<input::Written> {
    let unwritten = |reply| input::Written {
        reply,
        detail: String::new(),
    };
    let segments = segments(path);
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    match segments[..] {
        ["agents", project, scope, agent, action] => {
            let agent = match find_agent(config, project, scope, agent)? {
                Ok(agent) => agent,
                Err(reply) => return Ok(unwritten(reply)),
            };
            if action != "restart" {
                return input::post(&agent, action, body, config.tmux_server.as_deref());
            }
            lifecycle::restart(config, &agent, body)
        }
        ["features", project, feature, action @ ("merge" | "delete")] => {
            let root = match project_root(config, project)? {
                Ok(root) => root,
                Err(missing) => return Ok(unwritten(error(404, missing))),
            };
            if !has_feature(&root, feature)? {
                return Ok(unwritten(error(404, "no such feature")));
            }
            lifecycle::feature(config, &root, feature, action)
        }
        _ => Ok(unwritten(error(404, "no such endpoint"))),
    }
}

/// `/v1/projects/{project}/notes`, the project's notes, and
/// `POST /v1/projects/{project}/{open|close|delete}`; with what a write did,
/// for the request log.
fn projects_route(
    config: &Config,
    method: &str,
    path: &str,
    if_match: Option<&str>,
    body: &str,
) -> Result<(Reply, Option<String>)> {
    let segments = segments(path);
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let (project, action) = match segments[..] {
        [
            "projects",
            project,
            action @ ("notes" | "open" | "close" | "delete"),
        ] => (project, action),
        _ => return Ok((error(404, "no such endpoint"), None)),
    };
    if action != "notes" && method != "POST" {
        return Ok((error(405, "no such endpoint for this method"), None));
    }
    if action == "delete" {
        if !registered(config, project)? {
            return Ok((error(404, "no such project"), None));
        }
        let written = lifecycle::delete_project(config, project)?;
        return Ok((written.reply, Some(written.detail)));
    }
    let root = match project_root(config, project)? {
        Ok(root) => root,
        Err(missing) => return Ok((error(404, missing), None)),
    };
    if action != "notes" {
        let written = lifecycle::project(config, &root, action)?;
        return Ok((written.reply, Some(written.detail)));
    }
    match method {
        "GET" => Ok((notes::get(&root)?, None)),
        "PUT" => notes::put(&root, if_match, body),
        _ => Ok((error(405, "GET and PUT are served here"), None)),
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
        etag: None,
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

/// `token_sha256` is the SHA-256 of the request's token, which an event
/// stream is held to.
fn get(config: &Config, path: &str, query: &Query, token_sha256: &str) -> Result<Reply> {
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
                return Ok(Reply::Events {
                    token_sha256: token_sha256.to_string(),
                    watch: None,
                });
            };
            let [project, scope, agent] = watch.split('/').collect::<Vec<_>>()[..] else {
                return Ok(error(400, "watch names <project>/<scope>/<agent>"));
            };
            Ok(match find_agent(config, project, scope, agent)? {
                Ok(agent) => Reply::Events {
                    token_sha256: token_sha256.to_string(),
                    watch: Some(Box::new(TranscriptWatch::new(
                        agent,
                        query.get("after").map(str::to_string),
                    ))),
                },
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
            let root = match project_root(config, project)? {
                Ok(root) => root,
                Err(missing) => return Ok(error(404, missing)),
            };
            if !has_feature(&root, feature)? {
                return Ok(error(404, "no such feature"));
            }
            let info = feat_info::info(&root, &config.projects_dir, feature)?;
            Ok(ok(JSON, serde_json::to_string(&info)?))
        }
        ["features", project, feature, "merge"] => {
            let root = match project_root(config, project)? {
                Ok(root) => root,
                Err(missing) => return Ok(error(404, missing)),
            };
            if !has_feature(&root, feature)? {
                return Ok(error(404, "no such feature"));
            }
            let reason = feat_merge::blocker(&root, &config.projects_dir, feature)?;
            Ok(json(
                200,
                serde_json::json!({ "mergeable": reason.is_none(), "reason": reason }),
            ))
        }
        ["features", project, feature, "summary"] => {
            let root = match project_root(config, project)? {
                Ok(root) => root,
                Err(missing) => return Ok(error(404, missing)),
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
        [
            "agents",
            project,
            scope,
            agent,
            which @ ("dialog" | "dialogs"),
        ] => match find_agent(config, project, scope, agent)? {
            Ok(agent) => super::dialog::get(&agent, which == "dialogs"),
            Err(reply) => Ok(reply),
        },
        ["agents", project, scope, agent, "screen"] => {
            let root = match project_root(config, project)? {
                Ok(root) => root,
                Err(missing) => return Ok(error(404, missing)),
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
    let root = match project_root(config, project)? {
        Ok(root) => root,
        Err(missing) => return Ok(Err(error(404, missing))),
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

fn registered(config: &Config, name: &str) -> Result<bool> {
    Ok(ProjectEntry::scan(&config.projects_dir)?
        .projects
        .iter()
        .any(|(n, _)| n == name))
}

/// The root of the registered project named `name`, or the 404 message
/// when it isn't registered or isn't on this machine.
fn project_root(config: &Config, name: &str) -> Result<std::result::Result<PathBuf, &'static str>> {
    let found = ProjectEntry::scan(&config.projects_dir)?
        .projects
        .into_iter()
        .find(|(n, _)| n == name);
    Ok(match found {
        None => Err("no such project"),
        Some((_, entry)) if !entry.presence().is_here() => Err("project not on this machine"),
        Some((_, entry)) => Ok(entry.root_path()),
    })
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
