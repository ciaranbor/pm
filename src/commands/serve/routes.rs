//! What each request gets. The token is checked before anything else, so
//! a request without a valid one learns nothing, not even which paths
//! exist; a valid token without the endpoint's scope is refused after.
//! Path segments name only what the registry and pm state list, so none
//! reaches the filesystem as a path of its own.

use std::path::PathBuf;

use crate::commands::attention;
use crate::error::Result;
use crate::state::devices::{Devices, Scope};
use crate::state::feature::FeatureState;
use crate::state::paths;
use crate::state::project::ProjectEntry;
use crate::tmux;

use super::Config;

pub(super) enum Reply {
    Body {
        status: u16,
        content_type: &'static str,
        body: String,
    },
    /// Hand the connection to an event stream.
    Events,
}

impl Reply {
    pub(super) fn status(&self) -> u16 {
        match self {
            Self::Body { status, .. } => *status,
            Self::Events => 200,
        }
    }
}

pub(super) struct Handled {
    /// The device whose token the request carried.
    pub device: Option<String>,
    pub reply: Reply,
}

const JSON: &str = "application/json";
const MARKDOWN: &str = "text/markdown; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

fn error(status: u16, message: &str) -> Reply {
    Reply::Body {
        status,
        content_type: JSON,
        body: serde_json::json!({ "error": message }).to_string(),
    }
}

fn ok(content_type: &'static str, body: String) -> Reply {
    Reply::Body {
        status: 200,
        content_type,
        body,
    }
}

pub(super) fn route(
    config: &Config,
    method: &str,
    path: &str,
    authorization: Option<&str>,
) -> Handled {
    let token = authorization.and_then(bearer);
    let devices = match Devices::load(&config.devices) {
        Ok(devices) => devices,
        Err(e) => {
            return Handled {
                device: None,
                reply: error(500, &format!("devices unreadable: {e}")),
            };
        }
    };
    let Some((device, paired)) = token.and_then(|t| devices.authenticate(t.trim())) else {
        return Handled {
            device: None,
            reply: error(401, "a paired device's bearer token is required"),
        };
    };
    let reply = if method != "GET" {
        error(405, "only GET is served")
    } else if !paired.scopes.contains(&Scope::Read) {
        error(403, "this device's token lacks the read scope")
    } else {
        get(config, path).unwrap_or_else(|e| error(500, &e.to_string()))
    };
    Handled {
        device: Some(device.to_string()),
        reply,
    }
}

/// The token of a `Bearer` credential; the scheme is case-insensitive.
fn bearer(authorization: &str) -> Option<&str> {
    let (scheme, token) = authorization.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token)
}

fn get(config: &Config, path: &str) -> Result<Reply> {
    let segments: Option<Vec<String>> = path
        .strip_prefix("/v1/")
        .map(|rest| rest.split('/').map(decode).collect());
    let segments = segments.unwrap_or_default();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let server = config.tmux_server.as_deref();
    match segments[..] {
        ["snapshot"] => {
            let snapshot = attention::all(&config.projects_dir, server)?;
            Ok(ok(JSON, serde_json::to_string(&snapshot)?))
        }
        ["events"] => Ok(Reply::Events),
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
