//! The **dialog record**: a dialog on an agent's screen that can be
//! answered remotely, written by `pm harness hooks dialog` while it blocks
//! the harness's own decision point, and the **answer** `pm serve` leaves
//! for it. One record per agent; a newer dialog's replaces it, which ends
//! the older dialog's hook, so that one is answered at the terminal only.
//!
//! The record is harness-neutral apart from `reply_context`, which holds
//! what the harness needs to turn an answer into its decision and is never
//! served. The answer is a separate file, taken by renaming it away, so of
//! the hook and a `pm serve` withdrawing it only one gets it.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{WaitingKind, agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const DIALOG_FILE: &str = "dialog.json";
const ANSWER_FILE: &str = "dialog-answer.json";

/// A dialog as a client sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dialog {
    pub id: String,
    /// `question`, `permission` or `plan`.
    pub kind: WaitingKind,
    pub since: DateTime<Utc>,
    /// The harness's id for the subagent whose dialog it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    /// The tool a permission prompt is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// What the tool would act on: the command, the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A plan's text, in Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
    /// Whether an answer of the user's own words is taken.
    pub custom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    /// Whether the choice takes a message for the agent.
    pub takes_message: bool,
}

impl Choice {
    pub fn new(id: &str, label: impl Into<String>, takes_message: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.into(),
            takes_message,
        }
    }
}

/// The choice that answers a dialog's questions.
pub const ANSWER_CHOICE: &str = "answer";

impl Dialog {
    /// A dialog of `kind` opening now, with a fresh id.
    pub fn new(kind: WaitingKind) -> Self {
        let mut bytes = [0u8; 8];
        let id = match getrandom::fill(&mut bytes) {
            Ok(()) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
            Err(_) => format!("{}-{}", std::process::id(), Utc::now().timestamp_micros()),
        };
        Self {
            id,
            kind,
            since: Utc::now(),
            subagent: None,
            questions: Vec::new(),
            tool: None,
            detail: None,
            plan: None,
            choices: Vec::new(),
        }
    }

    /// Why `answer` can't answer this dialog; `None` when it can. A choice
    /// that answers the questions needs every one answered: by its
    /// options' labels, one unless it is multi-select, or by the user's own
    /// words where it takes them.
    pub fn invalid(&self, answer: &Answer) -> Option<String> {
        if answer.id != self.id {
            return Some("the answer names another dialog".into());
        }
        let Some(choice) = self.choices.iter().find(|c| c.id == answer.choice) else {
            let ids: Vec<&str> = self.choices.iter().map(|c| c.id.as_str()).collect();
            return Some(format!("choice is one of: {}", ids.join(", ")));
        };
        if answer.message.is_some() && !choice.takes_message {
            return Some(format!("{} takes no message", choice.id));
        }
        if choice.id != ANSWER_CHOICE {
            return (!answer.answers.is_empty()).then(|| format!("{} takes no answers", choice.id));
        }
        if let Some(extra) = answer
            .answers
            .keys()
            .find(|k| !self.questions.iter().any(|q| &q.question == *k))
        {
            return Some(format!("no question reads {extra:?}"));
        }
        for q in &self.questions {
            let picked = answer
                .answers
                .get(&q.question)
                .map_or(&[][..], Vec::as_slice);
            if picked.is_empty() || picked.iter().any(|p| p.trim().is_empty()) {
                return Some(format!("{:?} is unanswered", q.question));
            }
            if picked.len() > 1 && !q.multi_select {
                return Some(format!("{:?} takes one answer", q.question));
            }
            let own = picked
                .iter()
                .filter(|p| !q.options.iter().any(|o| &o.label == *p))
                .count();
            if own > 0 && !q.custom {
                return Some(format!("{:?} takes only its options", q.question));
            }
        }
        None
    }
}

/// A dialog as its hook records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DialogRecord {
    #[serde(flatten)]
    pub dialog: Dialog,
    /// The hook's process, which answers it.
    pub pid: u32,
    /// What the harness needs to build its decision; never served.
    pub reply_context: Value,
}

/// The user's answer to a dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The dialog it answers.
    pub id: String,
    pub choice: String,
    /// The labels picked, or words typed, per question text.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answers: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn write_dialog(
    project_root: &Path,
    scope: &str,
    agent: &str,
    record: &DialogRecord,
) -> Result<()> {
    let file = agent_dir(project_root, scope, agent)?.join(DIALOG_FILE);
    write_atomic(&file, serde_json::to_string(record)?.as_bytes())
}

/// The agent's dialog record; `None` when it has none or it can't be read.
pub fn read_dialog(project_root: &Path, scope: &str, agent: &str) -> Option<DialogRecord> {
    let text = std::fs::read_to_string(agent_file(project_root, scope, agent, DIALOG_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the agent's dialog record if it is dialog `id`'s, with any
/// answer left for it.
pub fn remove_dialog(project_root: &Path, scope: &str, agent: &str, id: &str) -> Result<()> {
    if read_dialog(project_root, scope, agent).is_some_and(|r| r.dialog.id == id) {
        remove(&agent_file(project_root, scope, agent, DIALOG_FILE))?;
    }
    let answer = agent_file(project_root, scope, agent, ANSWER_FILE);
    let stale = std::fs::read_to_string(&answer)
        .ok()
        .and_then(|t| serde_json::from_str::<Answer>(&t).ok())
        .is_some_and(|a| a.id == id);
    if stale {
        remove(&answer)?;
    }
    Ok(())
}

/// Leave `answer` for the dialog's hook, unless an answer is already left:
/// false then, so of two answers the first stands. Linked into place
/// whole, so the hook never reads it half written.
pub fn leave_answer(
    project_root: &Path,
    scope: &str,
    agent: &str,
    answer: &Answer,
) -> Result<bool> {
    let dir = agent_dir(project_root, scope, agent)?;
    static LEFT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let staged = dir.join(format!(
        "{ANSWER_FILE}.new-{}-{}",
        std::process::id(),
        LEFT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    write_atomic(&staged, serde_json::to_string(answer)?.as_bytes())?;
    let linked = std::fs::hard_link(&staged, dir.join(ANSWER_FILE));
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Remove the answer left for the agent's dialog, returning it, so of two
/// callers only one takes it. One that can't be read is dropped.
pub fn take_answer(project_root: &Path, scope: &str, agent: &str) -> Result<Option<Answer>> {
    let file = agent_file(project_root, scope, agent, ANSWER_FILE);
    static TAKES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let taken = file.with_file_name(format!(
        "{ANSWER_FILE}.taken-{}-{}",
        std::process::id(),
        TAKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    match std::fs::rename(&file, &taken) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let text = std::fs::read_to_string(&taken);
    let _ = std::fs::remove_file(&taken);
    Ok(text.ok().and_then(|t| serde_json::from_str(&t).ok()))
}

/// Whether an answer is waiting to be taken.
pub fn answer_pending(project_root: &Path, scope: &str, agent: &str) -> bool {
    agent_file(project_root, scope, agent, ANSWER_FILE).exists()
}

fn remove(file: &Path) -> Result<()> {
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn questions() -> Dialog {
        let option = |label: &str| QuestionOption {
            label: label.into(),
            description: String::new(),
        };
        Dialog {
            questions: vec![
                Question {
                    question: "Which DB?".into(),
                    header: "DB".into(),
                    options: vec![option("Postgres"), option("SQLite")],
                    multi_select: false,
                    custom: true,
                },
                Question {
                    question: "Which features?".into(),
                    header: "Features".into(),
                    options: vec![option("Auth"), option("Search")],
                    multi_select: true,
                    custom: false,
                },
            ],
            choices: vec![
                Choice::new(ANSWER_CHOICE, "Submit", false),
                Choice::new("decline", "Decline", false),
            ],
            ..Dialog::new(WaitingKind::Question)
        }
    }

    fn answer(dialog: &Dialog, choice: &str, answers: &[(&str, &[&str])]) -> Answer {
        Answer {
            id: dialog.id.clone(),
            choice: choice.into(),
            answers: answers
                .iter()
                .map(|(q, a)| (q.to_string(), a.iter().map(|s| s.to_string()).collect()))
                .collect(),
            message: None,
        }
    }

    #[test]
    fn an_answer_must_fit_the_dialogs_questions_and_choices() {
        let d = questions();
        let valid = [
            ("Which DB?", &["Postgres"][..]),
            ("Which features?", &["Auth", "Search"][..]),
        ];
        assert_eq!(d.invalid(&answer(&d, ANSWER_CHOICE, &valid)), None);
        let own_words = [
            ("Which DB?", &["DuckDB, embedded"][..]),
            ("Which features?", &["Auth"][..]),
        ];
        assert_eq!(d.invalid(&answer(&d, ANSWER_CHOICE, &own_words)), None);
        assert_eq!(d.invalid(&answer(&d, "decline", &[])), None);

        type Answers<'a> = &'a [(&'a str, &'a [&'a str])];
        let invalid: [(&str, Answers); 6] = [
            ("maybe", &[]),
            (ANSWER_CHOICE, &[("Which DB?", &["Postgres"])]),
            (
                ANSWER_CHOICE,
                &[
                    ("Which DB?", &["Postgres", "SQLite"]),
                    ("Which features?", &["Auth"]),
                ],
            ),
            (
                ANSWER_CHOICE,
                &[
                    ("Which DB?", &["SQLite"]),
                    ("Which features?", &["Billing"]),
                ],
            ),
            (
                ANSWER_CHOICE,
                &[
                    ("Which DB?", &["SQLite"]),
                    ("Which features?", &["Auth"]),
                    ("Which OS?", &["Linux"]),
                ],
            ),
            ("decline", &[("Which DB?", &["SQLite"])]),
        ];
        for (choice, answers) in invalid {
            assert!(
                d.invalid(&answer(&d, choice, answers)).is_some(),
                "{choice} {answers:?}"
            );
        }

        let mut stale = answer(&d, ANSWER_CHOICE, &valid);
        stale.id = "other".into();
        assert!(d.invalid(&stale).is_some());
        let mut message = answer(&d, "decline", &[]);
        message.message = Some("no".into());
        assert!(d.invalid(&message).is_some(), "decline takes no message");
    }

    #[test]
    fn a_record_is_removed_only_by_its_own_dialog_and_the_first_answer_is_taken_once() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let record = DialogRecord {
            dialog: questions(),
            pid: 1,
            reply_context: serde_json::json!({"secret": true}),
        };
        let id = record.dialog.id.clone();
        write_dialog(root, "login", "qa", &record).unwrap();
        assert_eq!(read_dialog(root, "login", "qa"), Some(record.clone()));

        remove_dialog(root, "login", "qa", "another").unwrap();
        assert!(read_dialog(root, "login", "qa").is_some());

        let given = answer(&record.dialog, "decline", &[]);
        assert!(leave_answer(root, "login", "qa", &given).unwrap());
        assert!(answer_pending(root, "login", "qa"));
        let second = answer(&record.dialog, ANSWER_CHOICE, &[("Which DB?", &["SQLite"])]);
        assert!(
            !leave_answer(root, "login", "qa", &second).unwrap(),
            "the first stands"
        );
        assert_eq!(
            take_answer(root, "login", "qa").unwrap(),
            Some(given.clone())
        );
        assert_eq!(take_answer(root, "login", "qa").unwrap(), None);

        assert!(leave_answer(root, "login", "qa", &given).unwrap());
        remove_dialog(root, "login", "qa", &id).unwrap();
        assert_eq!(read_dialog(root, "login", "qa"), None);
        assert!(!answer_pending(root, "login", "qa"));
    }
}
