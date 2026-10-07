//! The **dialog records**: the dialogs on an agent's screen that can be
//! answered remotely, each written by the `pm harness hooks dialog` that
//! blocks the harness's own decision point for it, or, for a dialog its
//! harness takes an answer to as typed input, by the waiting hook; and the
//! **answer** `pm serve` leaves for one. Several can be open at once (parallel subagents,
//! several opencode asks), each with its own record and answer.
//!
//! A record is harness-neutral apart from `reply_context`, which holds what
//! the harness needs to turn an answer into its decision and is never
//! served. An answer is a separate file, taken by renaming it away, so of
//! the hook and a `pm serve` withdrawing it only one gets it. Closing a
//! dialog leaves a tombstone for a day, so an answer naming a dialog that
//! has closed is told apart from one naming no dialog at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Waiting, WaitingKind, agent_dir, agent_file};
use crate::error::Result;
use crate::fs_utils::write_atomic;

const DIALOGS_DIR: &str = "dialogs";
const RECORD: &str = ".json";
const ANSWER: &str = ".answer.json";
const TOMBSTONE: &str = ".closed";
/// The tombstone of a dialog whose hook took an answer.
const TAKEN: &str = ".answered";
/// How long a closed dialog's id is remembered.
const TOMBSTONE_KEPT: Duration = Duration::from_secs(24 * 60 * 60);

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

    /// The waiting marker that stands for this dialog.
    pub fn waiting(&self) -> Waiting {
        let detail = match (&self.tool, &self.detail) {
            (Some(tool), Some(detail)) => {
                Some(format!("{tool}: {}", crate::harness::one_line(detail)))
            }
            (tool, detail) => detail.clone().or_else(|| tool.clone()),
        };
        Waiting {
            subagent: self.subagent.clone(),
            since: self.since,
            ..Waiting::now(self.kind, detail)
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
    /// The hook's process, which answers it; `None` for a dialog no hook
    /// holds, answered by typing its harness's reply
    /// ([`Harness::typed_reply`](crate::harness::Harness::typed_reply)).
    #[serde(default)]
    pub pid: Option<u32>,
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

/// The agent's dialogs dir, not created.
fn dialogs_dir(project_root: &Path, scope: &str, agent: &str) -> PathBuf {
    agent_file(project_root, scope, agent, DIALOGS_DIR)
}

/// The file `suffix` of dialog `id`; `None` for an id no dialog could have,
/// so a client's id never names a path.
fn dialog_file(
    project_root: &Path,
    scope: &str,
    agent: &str,
    id: &str,
    suffix: &str,
) -> Option<PathBuf> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    valid.then(|| dialogs_dir(project_root, scope, agent).join(format!("{id}{suffix}")))
}

/// Record a dialog that opened, pruning tombstones older than a day.
pub fn write_dialog(
    project_root: &Path,
    scope: &str,
    agent: &str,
    record: &DialogRecord,
) -> Result<()> {
    let dir = agent_dir(project_root, scope, agent)?.join(DIALOGS_DIR);
    std::fs::create_dir_all(&dir)?;
    prune_tombstones(&dir);
    let file = dir.join(format!("{}{RECORD}", record.dialog.id));
    write_atomic(&file, serde_json::to_string(record)?.as_bytes())
}

fn prune_tombstones(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > TOMBSTONE_KEPT);
        let name = entry.file_name().to_string_lossy().into_owned();
        if old && (name.ends_with(TOMBSTONE) || name.ends_with(TAKEN)) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Dialog `id`'s record while it is open; `None` once closed, or when it
/// can't be read.
pub fn read_dialog(
    project_root: &Path,
    scope: &str,
    agent: &str,
    id: &str,
) -> Option<DialogRecord> {
    let file = dialog_file(project_root, scope, agent, id, RECORD)?;
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// The agent's open dialog records, oldest first.
pub fn read_dialogs(project_root: &Path, scope: &str, agent: &str) -> Vec<DialogRecord> {
    let Ok(entries) = std::fs::read_dir(dialogs_dir(project_root, scope, agent)) else {
        return Vec::new();
    };
    let mut records: Vec<DialogRecord> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(RECORD))
        .filter(|e| !e.file_name().to_string_lossy().ends_with(ANSWER))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    records.sort_by(|a: &DialogRecord, b| {
        (a.dialog.since, &a.dialog.id).cmp(&(b.dialog.since, &b.dialog.id))
    });
    records
}

/// Close dialog `id`: its record becomes a tombstone, so an answer naming
/// it is known to come too late, and any answer left for it goes. `taken`
/// says its hook took an answer, which the tombstone records before the
/// record goes, so whoever left that answer learns it won.
pub fn close_dialog(
    project_root: &Path,
    scope: &str,
    agent: &str,
    id: &str,
    taken: bool,
) -> Result<()> {
    let (Some(record), Some(closed), Some(won), Some(answer)) = (
        dialog_file(project_root, scope, agent, id, RECORD),
        dialog_file(project_root, scope, agent, id, TOMBSTONE),
        dialog_file(project_root, scope, agent, id, TAKEN),
        dialog_file(project_root, scope, agent, id, ANSWER),
    ) else {
        return Ok(());
    };
    if taken {
        std::fs::write(&won, b"")?;
        remove(&record)?;
        remove(&closed)?;
    } else if !won.exists() {
        match std::fs::rename(&record, &closed) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    remove(&answer)
}

/// Whether dialog `id` was open once and has closed since.
pub fn dialog_closed(project_root: &Path, scope: &str, agent: &str, id: &str) -> bool {
    [TOMBSTONE, TAKEN]
        .iter()
        .any(|t| dialog_file(project_root, scope, agent, id, t).is_some_and(|f| f.exists()))
}

/// Whether dialog `id` closed with its hook taking an answer.
pub fn dialog_answer_taken(project_root: &Path, scope: &str, agent: &str, id: &str) -> bool {
    dialog_file(project_root, scope, agent, id, TAKEN).is_some_and(|f| f.exists())
}

/// Leave `answer` for its dialog's hook, unless an answer is already left
/// for it: false then, so of two answers the first stands. Linked into
/// place whole, so the hook never reads it half written.
pub fn leave_answer(
    project_root: &Path,
    scope: &str,
    agent: &str,
    answer: &Answer,
) -> Result<bool> {
    let Some(file) = dialog_file(project_root, scope, agent, &answer.id, ANSWER) else {
        return Ok(false);
    };
    let dir = agent_dir(project_root, scope, agent)?.join(DIALOGS_DIR);
    std::fs::create_dir_all(&dir)?;
    static LEFT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let staged = dir.join(format!(
        ".answer.new-{}-{}",
        std::process::id(),
        LEFT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    write_atomic(&staged, serde_json::to_string(answer)?.as_bytes())?;
    let linked = std::fs::hard_link(&staged, file);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Remove the answer left for dialog `id`, returning it, so of two callers
/// only one takes it. One that can't be read is dropped.
pub fn take_answer(
    project_root: &Path,
    scope: &str,
    agent: &str,
    id: &str,
) -> Result<Option<Answer>> {
    let Some(file) = dialog_file(project_root, scope, agent, id, ANSWER) else {
        return Ok(None);
    };
    static TAKES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let taken = file.with_file_name(format!(
        ".answer.taken-{}-{}",
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
    Ok(text
        .ok()
        .and_then(|t| serde_json::from_str::<Answer>(&t).ok())
        .filter(|a| a.id == id))
}

/// Whether an answer is waiting for dialog `id`'s hook to take it.
pub fn answer_pending(project_root: &Path, scope: &str, agent: &str, id: &str) -> bool {
    dialog_file(project_root, scope, agent, id, ANSWER).is_some_and(|f| f.exists())
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
    fn dialogs_open_side_by_side_each_taking_only_its_own_first_answer() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let record = |dialog: Dialog| DialogRecord {
            dialog,
            pid: Some(1),
            reply_context: serde_json::json!({"secret": true}),
        };
        let older = record(questions());
        let mut newer = record(questions());
        newer.dialog.since = older.dialog.since + chrono::Duration::seconds(1);
        write_dialog(root, "login", "qa", &newer).unwrap();
        write_dialog(root, "login", "qa", &older).unwrap();
        assert_eq!(
            read_dialogs(root, "login", "qa"),
            [older.clone(), newer.clone()]
        );

        let given = answer(&newer.dialog, "decline", &[]);
        assert!(leave_answer(root, "login", "qa", &given).unwrap());
        let second = answer(&newer.dialog, ANSWER_CHOICE, &[("Which DB?", &["SQLite"])]);
        assert!(
            !leave_answer(root, "login", "qa", &second).unwrap(),
            "the first stands"
        );
        assert_eq!(
            take_answer(root, "login", "qa", &older.dialog.id).unwrap(),
            None
        );
        assert_eq!(
            take_answer(root, "login", "qa", &newer.dialog.id).unwrap(),
            Some(given.clone())
        );
        assert_eq!(
            take_answer(root, "login", "qa", &newer.dialog.id).unwrap(),
            None
        );

        assert!(leave_answer(root, "login", "qa", &given).unwrap());
        close_dialog(root, "login", "qa", &newer.dialog.id, false).unwrap();
        assert!(!answer_pending(root, "login", "qa", &newer.dialog.id));
        assert_eq!(
            read_dialogs(root, "login", "qa"),
            std::slice::from_ref(&older)
        );
        assert!(dialog_closed(root, "login", "qa", &newer.dialog.id));
        assert!(!dialog_closed(root, "login", "qa", &older.dialog.id));
        assert!(!dialog_closed(root, "login", "qa", "unknown"));
    }

    #[test]
    fn an_id_that_is_not_a_dialogs_names_no_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut escape = answer(&questions(), "decline", &[]);
        escape.id = "../../waiting".into();
        assert!(!leave_answer(root, "login", "qa", &escape).unwrap());
        assert_eq!(read_dialog(root, "login", "qa", "../../waiting"), None);
        assert!(!root.join(".pm").exists(), "nothing written");
    }
}
