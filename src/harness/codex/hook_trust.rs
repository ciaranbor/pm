//! Whether codex will run pm's hooks. Codex records trust per hook as a
//! hash of the hook's normalized definition (`hooks.state.<key>.trusted_hash`
//! in `config.toml`), so a pm update that changes a hook's command leaves an
//! entry codex no longer honours: it asks again at the next start and runs
//! nothing until then. The hash is codex's own fingerprint of its internal
//! TOML form, so pm asks codex instead of recomputing it — the app-server's
//! `hooks/list` (present since 0.156) answers `trusted`, `modified` or
//! `untrusted` per hook. That costs a `codex app-server` start, so it runs
//! only for a fresh probe; otherwise, and when codex gives no answer, an
//! entry's presence is taken as trust.
//!
//! With the user's consent pm also records that trust ([`trust_pm_hooks`]),
//! through the same server: `hooks/list` reports each hook's `currentHash`
//! (0.160 has it), and `config/batchWrite` upserts it under `hooks.state`,
//! byte-identical to what the TUI's "Trust all and continue" writes. Only
//! keys at pm's own hooks' positions are written, so a user's hook stays
//! theirs to trust.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

use serde_json::{Value, json};

use super::{BINARY, HOOKS_FILE, MANUAL_TRUST, hook_trust_key, hook_trusted};
use crate::error::{PmError, Result};
use crate::harness::HookTrustStatus;

/// Codex's trust in each hook of its user-level hooks file.
pub(in crate::harness) struct HookTrust {
    codex_home: PathBuf,
    /// `hooks/list`'s answer by hook key; `None` when codex was not asked or
    /// did not answer.
    statuses: Option<HashMap<String, String>>,
}

impl HookTrust {
    pub(in crate::harness) fn read(codex_home: &Path, ask_codex: bool) -> Self {
        Self {
            codex_home: codex_home.to_path_buf(),
            // A lib test must never start the developer's own codex.
            statuses: (ask_codex && !cfg!(test))
                .then(|| list(codex_home))
                .flatten(),
        }
    }

    pub(in crate::harness) fn status(
        &self,
        event: &str,
        entry: usize,
        hook: usize,
    ) -> HookTrustStatus {
        let key = hook_trust_key(&self.codex_home.join(HOOKS_FILE), event, entry, hook);
        match self.statuses.as_ref().and_then(|s| s.get(&key)) {
            Some(status) => parse_status(status),
            None if hook_trusted(&self.codex_home, &key) => HookTrustStatus::Trusted,
            None => HookTrustStatus::Untrusted,
        }
    }
}

fn parse_status(status: &str) -> HookTrustStatus {
    match status {
        "trusted" | "managed" => HookTrustStatus::Trusted,
        "modified" => HookTrustStatus::Modified,
        _ => HookTrustStatus::Untrusted,
    }
}

/// Trust pm's hooks, at `positions` (event, entry, inner hook) in the hooks
/// file. Codex is asked only with `recheck` or when some hook has no trust
/// entry: an entry outdated by a changed hook goes unseen without it.
/// `None` when nothing was trusted, codex not being installed included.
pub(in crate::harness) fn trust_pm_hooks(
    codex_home: &Path,
    positions: &[(&str, usize, usize)],
    recheck: bool,
) -> Result<Option<String>> {
    let hooks_file = codex_home.join(HOOKS_FILE);
    let keys: Vec<String> = positions
        .iter()
        .map(|(event, entry, hook)| hook_trust_key(&hooks_file, event, *entry, *hook))
        .collect();
    if keys.is_empty() || !recheck && keys.iter().all(|key| hook_trusted(codex_home, key)) {
        return Ok(None);
    }
    let why = match trust(codex_home, &keys) {
        Ok(trusted) if trusted.is_empty() => return Ok(None),
        Ok(trusted) => {
            return Ok(Some(format!(
                "Trusted {} pm hook{} in codex",
                trusted.len(),
                if trusted.len() == 1 { "" } else { "s" }
            )));
        }
        Err(Refusal::NotInstalled) => return Ok(None),
        Err(Refusal::NoAnswer) => "codex did not answer".to_string(),
        Err(Refusal::Unlisted(key)) => format!("codex lists no hash for {key}"),
        Err(Refusal::Refused(reason)) => format!("codex refused: {reason}"),
    };
    Err(PmError::HookTrust(format!(
        "could not trust pm's hooks in codex ({why}): {MANUAL_TRUST}"
    )))
}

/// Record codex's trust in the hooks at `keys` — pm's own — that it does
/// not trust as they stand, with the hash codex reports for each, through
/// codex's own config writer (`config/batchWrite`, the entries the TUI's
/// "Trust all and continue" writes). A hook at any other key is never
/// touched. Returns the keys trusted.
fn trust(codex_home: &Path, keys: &[String]) -> std::result::Result<Vec<String>, Refusal> {
    if cfg!(test) {
        return Err(Refusal::NotInstalled);
    }
    let mut session = Session::open(codex_home)?;
    let listed = session.call("hooks/list", json!({"cwds": [session.cwd]}))?;
    let edits = trust_edits(&listed, keys)?;
    let Some(value) = edits.value else {
        return Ok(Vec::new());
    };
    let written = session.call(
        "config/batchWrite",
        json!({"edits": [{"keyPath": "hooks.state", "value": value, "mergeStrategy": "upsert"}]}),
    )?;
    if written.get("result").is_none() {
        return Err(Refusal::Refused(
            written
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("no result")
                .to_string(),
        ));
    }
    Ok(edits.keys)
}

/// Why [`trust`] recorded nothing.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// No codex binary to ask.
    NotInstalled,
    /// Codex did not answer.
    NoAnswer,
    /// Codex lists no hook at this key, or no hash for it.
    Unlisted(String),
    /// Codex refused the write, with its reason.
    Refused(String),
}

/// The `hooks.state` entries that trust `keys` as `hooks/list` answered,
/// and the keys they trust; `None` when every one is trusted already.
struct Edits {
    value: Option<Value>,
    keys: Vec<String>,
}

fn trust_edits(listed: &Value, keys: &[String]) -> std::result::Result<Edits, Refusal> {
    let hooks = listed_hooks(listed).ok_or(Refusal::NoAnswer)?;
    let mut state = serde_json::Map::new();
    let mut trusted = Vec::new();
    for key in keys {
        let hook = hooks
            .iter()
            .find(|hook| hook["key"] == key.as_str())
            .ok_or_else(|| Refusal::Unlisted(key.clone()))?;
        if parse_status(hook["trustStatus"].as_str().unwrap_or_default())
            == HookTrustStatus::Trusted
        {
            continue;
        }
        let hash = hook["currentHash"]
            .as_str()
            .ok_or_else(|| Refusal::Unlisted(key.clone()))?;
        state.insert(key.clone(), json!({"trusted_hash": hash}));
        trusted.push(key.clone());
    }
    Ok(Edits {
        value: (!state.is_empty()).then_some(Value::Object(state)),
        keys: trusted,
    })
}

/// Kills the app-server's process group when dropped.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        // Safety: signalling a process group pm created; a gone group only
        // makes kill fail.
        unsafe {
            libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

/// A `codex app-server` of pm's own over stdio, initialized.
struct Session {
    cwd: PathBuf,
    stdin: ChildStdin,
    replies: mpsc::Receiver<Value>,
    next_id: u64,
    // Last, so the pipes close before the group is killed.
    _server: Server,
}

impl Session {
    fn open(codex_home: &Path) -> std::result::Result<Self, Refusal> {
        let cwd = codex_home.parent().unwrap_or(codex_home).to_path_buf();
        let mut child = Command::new(BINARY)
            .arg("app-server")
            .env("CODEX_HOME", codex_home)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Refusal::NotInstalled,
                _ => Refusal::NoAnswer,
            })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let server = Server(child);
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(Refusal::NoAnswer);
        };
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("id").is_some() && sender.send(message).is_err() {
                    return;
                }
            }
        });
        let mut session = Self {
            cwd,
            stdin,
            replies,
            next_id: 1,
            _server: server,
        };
        session.call(
            "initialize",
            json!({"clientInfo": {"name": "pm", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        session.send(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        Ok(session)
    }

    fn send(&mut self, message: &Value) -> std::result::Result<(), Refusal> {
        writeln!(self.stdin, "{message}").map_err(|_| Refusal::NoAnswer)
    }

    /// `method`'s reply, error replies included.
    fn call(&mut self, method: &str, params: Value) -> std::result::Result<Value, Refusal> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + crate::harness::CALL_LIMIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let reply = self
                .replies
                .recv_timeout(left)
                .map_err(|_| Refusal::NoAnswer)?;
            if reply["id"] == id {
                return Ok(reply);
            }
        }
    }
}

/// `hooks/list` over a `codex app-server` of pm's own, as hook key →
/// trust status.
fn list(codex_home: &Path) -> Option<HashMap<String, String>> {
    let mut session = Session::open(codex_home).ok()?;
    let cwd = session.cwd.clone();
    let reply = session.call("hooks/list", json!({"cwds": [cwd]})).ok()?;
    statuses(&reply)
}

/// Every hook `hooks/list` answered with.
fn listed_hooks(message: &Value) -> Option<Vec<&Value>> {
    let entries = message.pointer("/result/data")?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| entry["hooks"].as_array())
            .flatten()
            .collect(),
    )
}

fn statuses(message: &Value) -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    for hook in listed_hooks(message)? {
        if let (Some(key), Some(status)) = (hook["key"].as_str(), hook["trustStatus"].as_str()) {
            out.insert(key.to_string(), status.to_string());
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codexs_answer_outranks_the_entry_and_its_absence_falls_back_to_it() {
        let home = tempfile::tempdir().unwrap();
        let key = |event| hook_trust_key(&home.path().join(HOOKS_FILE), event, 0, 0);
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[hooks.state.\"{}\"]\ntrusted_hash = \"sha256:old\"\n\
                 [hooks.state.\"{}\"]\ntrusted_hash = \"sha256:x\"\n",
                key("Stop"),
                key("SessionStart")
            ),
        )
        .unwrap();
        let answer = json!({"id": 2, "result": {"data": [{"cwd": "/", "hooks": [
            {"key": key("Stop"), "eventName": "stop", "trustStatus": "modified"},
            {"key": key("Interrupt"), "eventName": "interrupt", "trustStatus": "untrusted"},
        ]}]}});
        let trust = HookTrust {
            codex_home: home.path().to_path_buf(),
            statuses: statuses(&answer),
        };
        assert_eq!(trust.status("Stop", 0, 0), HookTrustStatus::Modified);
        assert_eq!(trust.status("Interrupt", 0, 0), HookTrustStatus::Untrusted);
        assert_eq!(trust.status("SessionStart", 0, 0), HookTrustStatus::Trusted);
        assert_eq!(trust.status("PreToolUse", 0, 0), HookTrustStatus::Untrusted);

        let unasked = HookTrust::read(home.path(), false);
        assert_eq!(unasked.status("Stop", 0, 0), HookTrustStatus::Trusted);
    }

    #[test]
    fn trust_edits_cover_only_pms_hooks_codex_does_not_trust() {
        let hook = |key: &str, status: &str, hash: Option<&str>| {
            let mut hook = json!({"key": key, "trustStatus": status});
            if let Some(hash) = hash {
                hook["currentHash"] = json!(hash);
            }
            hook
        };
        let answer = json!({"id": 2, "result": {"data": [{"cwd": "/", "hooks": [
            hook("h:stop:1:0", "untrusted", Some("sha256:stop")),
            hook("h:session_start:0:0", "modified", Some("sha256:start")),
            hook("h:interrupt:0:0", "trusted", Some("sha256:int")),
            hook("h:stop:0:0", "untrusted", Some("sha256:users")),
        ]}]}});
        let pms = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect::<Vec<_>>();

        let edits = trust_edits(
            &answer,
            &pms(&["h:stop:1:0", "h:session_start:0:0", "h:interrupt:0:0"]),
        )
        .unwrap();
        assert_eq!(
            edits.value,
            Some(json!({
                "h:stop:1:0": {"trusted_hash": "sha256:stop"},
                "h:session_start:0:0": {"trusted_hash": "sha256:start"},
            }))
        );
        assert_eq!(edits.keys, pms(&["h:stop:1:0", "h:session_start:0:0"]));

        let trusted = trust_edits(&answer, &pms(&["h:interrupt:0:0"])).unwrap();
        assert_eq!(trusted.value, None);

        let hashless = json!({"result": {"data": [{"hooks": [
            hook("h:stop:1:0", "untrusted", None),
        ]}]}});
        assert_eq!(
            trust_edits(&hashless, &pms(&["h:stop:1:0"])).err(),
            Some(Refusal::Unlisted("h:stop:1:0".into()))
        );
        assert_eq!(
            trust_edits(&answer, &pms(&["h:guard:0:0"])).err(),
            Some(Refusal::Unlisted("h:guard:0:0".into()))
        );
        assert_eq!(
            trust_edits(&json!({"error": {"message": "no"}}), &pms(&["h:stop:1:0"])).err(),
            Some(Refusal::NoAnswer)
        );
    }
}
