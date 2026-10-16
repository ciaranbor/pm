//! Each agent's latest line: its last reply, read off the end of its
//! conversation through the harness's own reader and cut to one line, so a
//! view says what an agent last said without attaching to it.
//!
//! The views refresh often, so a read is kept per agent
//! ([`LatestRecord`]) with the conversation's version and reused while that
//! is unchanged: a refresh costs the version (a stat, or for opencode's
//! database one aggregate query) and a small file per agent.

use std::path::Path;

use crate::harness::transcript::items::Body;
use crate::state::agent::AgentEntry;
use crate::state::runtime::{self, Latest, LatestRecord};

/// The longest latest line the snapshot carries.
pub const MAX: usize = 200;

/// The longest the terminal views show it: the status rows, with the
/// agent's name, and pm's tmux tree.
pub const WIDTH: usize = 100;

/// Agent `name`'s latest line; `None` when it has no conversation, its
/// reader fails, or it has replied in none of what is read back.
pub(super) fn latest(
    project_root: &Path,
    scope: &str,
    name: &str,
    entry: &AgentEntry,
) -> Option<Latest> {
    let conversation = entry.conversation(project_root, scope, name).ok()??;
    let version = conversation.version();
    if let Some(version) = &version
        && let Some(record) = runtime::read_latest(project_root, scope, name)
        && &record.version == version
    {
        return record.latest;
    }
    let latest = conversation
        .last_reply()
        .ok()?
        .and_then(|item| match item.body {
            Body::Assistant { text } => Some(Latest {
                text: crate::text::ellipsize(&plain_line(&text), MAX),
                at: item.at,
            }),
            _ => None,
        })
        .filter(|l| !l.text.is_empty());
    if let Some(version) = version {
        let record = LatestRecord {
            version,
            latest: latest.clone(),
        };
        let _ = runtime::write_latest(project_root, scope, name, &record);
    }
    latest
}

/// Markdown `text` as one line of plain words: each line without its
/// quote, heading or list marker, bold and code ticks dropped, the lines
/// joined by a space.
fn plain_line(text: &str) -> String {
    let words: Vec<&str> = text
        .lines()
        .map(|line| {
            let line = line.trim_start().trim_start_matches('>').trim_start();
            let hashes = line.trim_start_matches('#');
            let line = match hashes.strip_prefix(' ') {
                Some(heading) if hashes.len() < line.len() => heading,
                _ => line,
            };
            let numbered = line.trim_start_matches(|c: char| c.is_ascii_digit());
            line.strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .or_else(|| {
                    (numbered.len() < line.len())
                        .then(|| numbered.strip_prefix(". "))
                        .flatten()
                })
                .unwrap_or(line)
        })
        .flat_map(str::split_whitespace)
        .collect();
    words.join(" ").replace("**", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::attention::scope_latest;
    use crate::state::agent::{AgentRegistry, AgentType};
    use crate::state::paths;
    use crate::state::runtime::SessionPath;
    use crate::testing::TestServer;
    use tempfile::tempdir;

    fn reply(uuid: &str, text: &str) -> String {
        serde_json::json!({"type": "assistant", "uuid": uuid,
            "timestamp": "2026-10-12T10:00:00Z",
            "message": {"content": [{"type": "text", "text": text}]}})
        .to_string()
            + "\n"
    }

    #[test]
    fn the_latest_line_follows_the_conversation_as_it_grows() {
        let dir = tempdir().unwrap();
        let (project, _, _) = TestServer::new().setup_project_no_tmux(dir.path());
        let agents_dir = paths::agents_dir(&project);
        let mut registry = AgentRegistry::load(&agents_dir, "main").unwrap();
        for (name, session) in [("worker", "s1"), ("fresh", "s2")] {
            registry.register(
                name,
                AgentEntry {
                    agent_type: AgentType::Agent,
                    session_id: session.to_string(),
                    window_name: name.to_string(),
                    active: true,
                    agent_definition: None,
                    harness: crate::harness::Harness::ClaudeCode,
                    spawned_at: None,
                },
            );
        }
        registry.save(&agents_dir, "main").unwrap();
        let transcript = dir.path().join("s1.jsonl");
        runtime::write_session_path(
            &project,
            "main",
            "worker",
            SessionPath::Transcript,
            Some(&transcript),
        )
        .unwrap();
        let latest = || {
            scope_latest(&project, "main")
                .unwrap()
                .into_iter()
                .map(|(agent, l)| (agent, l.text))
                .collect::<Vec<_>>()
        };
        std::fs::write(&transcript, reply("a1", "## Plan\nRead the **code**")).unwrap();
        assert_eq!(latest(), [("worker".into(), "Plan Read the code".into())]);

        let mut record = runtime::read_latest(&project, "main", "worker").unwrap();
        record.latest.as_mut().unwrap().text = "cached".into();
        runtime::write_latest(&project, "main", "worker", &record).unwrap();
        assert_eq!(
            latest(),
            [("worker".into(), "cached".into())],
            "an unchanged transcript is not read again"
        );

        let mut text = std::fs::read_to_string(&transcript).unwrap();
        text.push_str(&reply("a2", &"done ".repeat(100)));
        std::fs::write(&transcript, text).unwrap();
        let [(_, line)] = &latest()[..] else {
            panic!("{:?}", latest());
        };
        assert!(
            line.starts_with("done done") && line.ends_with('…'),
            "{line}"
        );
        assert!(line.chars().count() <= MAX);
    }

    #[test]
    fn a_reply_reads_as_one_line_of_words() {
        assert_eq!(
            plain_line("## Done\n\nFixed the **login** bug:\n- `cargo test` passes\n> note"),
            "Done Fixed the login bug: cargo test passes note"
        );
        assert_eq!(
            plain_line("#{session} #42 opens\n1. #7 first\n2024 was a year"),
            "#{session} #42 opens #7 first 2024 was a year"
        );
    }
}
