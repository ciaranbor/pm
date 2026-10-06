//! Whether codex will run pm's hooks. Codex records trust per hook as a
//! hash of the hook's normalized definition (`hooks.state.<key>.trusted_hash`
//! in `config.toml`), so a pm upgrade that changes a hook's command leaves an
//! entry codex no longer honours: it asks again at the next start and runs
//! nothing until then. The hash is codex's own fingerprint of its internal
//! TOML form, so pm asks codex instead of recomputing it — the app-server's
//! `hooks/list` (present since 0.156) answers `trusted`, `modified` or
//! `untrusted` per hook. That costs a `codex app-server` start, so it runs
//! only for a fresh probe; otherwise, and when codex gives no answer, an
//! entry's presence is taken as trust.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

use serde_json::{Value, json};

use super::{BINARY, HOOKS_FILE, hook_trust_key, hook_trusted};
use crate::harness::HookTrustStatus;

const LIST_ID: u64 = 2;

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

/// `hooks/list` over a `codex app-server` of pm's own, as hook key →
/// trust status.
fn list(codex_home: &Path) -> Option<HashMap<String, String>> {
    let cwd = codex_home.parent().unwrap_or(codex_home);
    let mut child = Command::new(BINARY)
        .arg("app-server")
        .env("CODEX_HOME", codex_home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take()?;
    let server = Server(child);

    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"clientInfo": {"name": "pm", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        json!({"jsonrpc": "2.0", "id": LIST_ID, "method": "hooks/list",
               "params": {"cwds": [cwd]}}),
    ];
    let mut stdin = stdin?;
    for request in requests {
        writeln!(stdin, "{request}").ok()?;
    }

    let (sender, answer) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"] == LIST_ID {
                let _ = sender.send(message);
                return;
            }
        }
    });
    let message = answer.recv_timeout(crate::harness::CALL_LIMIT).ok();
    drop(stdin);
    drop(server);
    statuses(&message?)
}

fn statuses(message: &Value) -> Option<HashMap<String, String>> {
    let entries = message.pointer("/result/data")?.as_array()?;
    let mut out = HashMap::new();
    for hook in entries
        .iter()
        .filter_map(|entry| entry["hooks"].as_array())
        .flatten()
    {
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
        // The shape codex 0.156–0.160 answers with.
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
}
