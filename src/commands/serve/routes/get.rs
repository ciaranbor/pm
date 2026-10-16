//! `GET` requests outside `/v1/projects/`: the snapshot, the event stream,
//! an agent's transcript, dialogs, mode and screen, and a feature's info,
//! mergeability and summary.

use crate::commands::serve::Config;
use crate::commands::serve::transcript::{DEFAULT_LIMIT, MAX_LIMIT, TranscriptWatch, page_json};
use crate::commands::{attention, feat_info, feat_merge};
use crate::error::Result;
use crate::state::{paths, scope};
use crate::tmux;

use super::{
    JSON, MARKDOWN, Reply, TEXT, decode, error, find_agent, has_feature, json, ok, project_root,
    segments,
};

/// A request's query parameters, decoded.
pub(super) struct Query(Vec<(String, String)>);

impl Query {
    pub(super) fn parse(query: &str) -> Self {
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
pub(super) fn get(config: &Config, path: &str, query: &Query, token_sha256: &str) -> Result<Reply> {
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
            Ok(agent) => crate::commands::serve::dialog::get(&agent, which == "dialogs"),
            Err(reply) => Ok(reply),
        },
        ["agents", project, scope, agent, "mode"] => {
            match find_agent(config, project, scope, agent)? {
                Ok(agent) => crate::commands::serve::mode::get(&agent, server),
                Err(reply) => Ok(reply),
            }
        }
        ["agents", project, scope, agent, "screen"] => {
            let root = match project_root(config, project)? {
                Ok(root) => root,
                Err(missing) => return Ok(error(404, missing)),
            };
            if scope != scope::MAIN && !has_feature(&root, scope)? {
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
