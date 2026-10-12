//! `pm agent log`: the end of an agent's conversation as plain text, read
//! through the same harness-neutral chat view `pm serve` hands the phone
//! app, so it reads any harness's sessions. Tool output and thinking are
//! cut to a few lines; what the agent and the human wrote is shown whole.

use std::fmt::Write;
use std::path::Path;

use chrono::{DateTime, Local, Utc};

use crate::error::{PmError, Result};
use crate::harness::transcript::items::{Body, Item};
use crate::state::agent::AgentRegistry;
use crate::state::paths;

/// Lines kept of a tool's output or a thought, and characters of each.
const CUT: usize = 5;
const WIDTH: usize = 200;

/// The last `count` entries of agent `name`'s conversation in `scope`.
pub fn log(project_root: &Path, scope: &str, name: &str, count: usize) -> Result<String> {
    let registry = AgentRegistry::load(&paths::agents_dir(project_root), scope)?;
    let Some(entry) = registry.get(name) else {
        return Err(PmError::Agent(format!(
            "no agent '{name}' in scope '{scope}'"
        )));
    };
    let Some(conversation) = entry.conversation(project_root, scope, name)? else {
        return Ok(format!("'{name}' has no conversation yet.\n"));
    };
    let page = conversation.page(None, count)?;
    let skip = page.items.len().saturating_sub(count);
    let today = Local::now().date_naive();
    let mut out = String::new();
    for (i, item) in page.items[skip..].iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render(&mut out, item, today);
    }
    if out.is_empty() {
        out = format!("'{name}' has an empty conversation.\n");
    }
    Ok(out)
}

fn render(out: &mut String, item: &Item, today: chrono::NaiveDate) {
    let at = item.at.map(|at| stamp(at, today));
    let head = |out: &mut String, label: &str, extra: &str| {
        let mut line = format!("── {label}");
        if let Some(at) = &at {
            write!(line, " · {at}").unwrap();
        }
        if !extra.is_empty() {
            write!(line, " · {extra}").unwrap();
        }
        writeln!(out, "{line}").unwrap();
    };
    match &item.body {
        Body::User { text } => {
            head(out, "user", "");
            body(out, text);
        }
        Body::Assistant { text } => {
            head(out, "assistant", "");
            body(out, text);
        }
        Body::Continuation { text } => {
            head(out, "continuation", "");
            body(out, text);
        }
        Body::Thinking { text } => {
            head(out, "thinking", "");
            cut(out, text);
        }
        Body::Tool {
            name,
            input,
            result,
            unfinished,
        } => {
            let label = match result {
                Some(r) if r.error => format!("{name} (error)"),
                Some(_) => name.clone(),
                None if *unfinished => format!("{name} (interrupted)"),
                None => format!("{name} (running)"),
            };
            head(out, &label, input);
            if let Some(result) = result {
                cut(out, &result.text);
            }
        }
        Body::Compaction { .. } => head(out, "context compacted", ""),
        Body::Event { text, failure } => {
            head(out, if *failure { "failure" } else { "event" }, "");
            body(out, text);
        }
    }
}

/// `at` in local time: the time alone when it is `today`.
fn stamp(at: DateTime<Utc>, today: chrono::NaiveDate) -> String {
    let local = at.with_timezone(&Local);
    if local.date_naive() == today {
        local.format("%H:%M").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

fn body(out: &mut String, text: &str) {
    let text = text.trim_end();
    if !text.is_empty() {
        writeln!(out, "{text}").unwrap();
    }
}

/// The first [`CUT`] lines of `text`, each cut to [`WIDTH`] characters,
/// then how many more there were.
fn cut(out: &mut String, text: &str) {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    for line in lines.iter().take(CUT) {
        match line.char_indices().nth(WIDTH) {
            Some((end, _)) => writeln!(out, "{}…", &line[..end]).unwrap(),
            None => writeln!(out, "{line}").unwrap(),
        }
    }
    if lines.len() > CUT {
        writeln!(out, "… {} more lines", lines.len() - CUT).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::log;
    use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
    use crate::state::paths;
    use crate::state::runtime::{self, SessionPath};
    use crate::testing::TestServer;

    #[test]
    fn the_last_entries_are_shown_with_long_output_cut() {
        let dir = tempdir().unwrap();
        let (project, _, _) = TestServer::new().setup_project_no_tmux(dir.path());
        let agents_dir = paths::agents_dir(&project);
        let mut registry = AgentRegistry::load(&agents_dir, "main").unwrap();
        registry.register(
            "worker",
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: "s1".to_string(),
                window_name: "worker".to_string(),
                active: true,
                agent_definition: None,
                harness: crate::harness::Harness::ClaudeCode,
                spawned_at: None,
            },
        );
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
        let mut output: Vec<String> = (1..=8).map(|i| format!("line {i}")).collect();
        output[0].push_str(&"x".repeat(300));
        let lines = [
            serde_json::json!({"type": "user", "uuid": "u1", "promptSource": "typed",
                "message": {"role": "user", "content": "too old to show"}}),
            serde_json::json!({"type": "user", "uuid": "u2", "promptSource": "typed",
                "message": {"role": "user", "content": "Fix the login bug"}}),
            serde_json::json!({"type": "assistant", "uuid": "a1",
                "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash",
                                         "input": {"command": "cargo test"}}]}}),
            serde_json::json!({"type": "user", "uuid": "r1",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1",
                                         "content": output.join("\n"), "is_error": true}]}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&transcript, text).unwrap();

        assert_eq!(
            log(&project, "main", "worker", 2).unwrap(),
            format!(
                "── user\nFix the login bug\n\n\
                 ── Bash (error) · cargo test\n\
                 line 1{}…\nline 2\nline 3\nline 4\nline 5\n… 3 more lines\n",
                "x".repeat(194)
            )
        );
        assert!(log(&project, "main", "nobody", 2).is_err());
    }
}
