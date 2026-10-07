//! A tmux control-mode client, for tests that watch what tmux tells clients.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

/// A control-mode client attached to a session, recording what tmux
/// sends it.
pub struct ControlClient {
    child: Child,
    stdin: ChildStdin,
    output: Arc<Mutex<Vec<String>>>,
    syncs: u32,
}

impl ControlClient {
    pub fn attach(server: Option<&str>, session: &str) -> Self {
        let mut child = Command::new("tmux")
            .args(["-L", server.unwrap(), "-C", "attach", "-t"])
            .arg(format!("={session}:"))
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let lines = Arc::clone(&output);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                lines.lock().unwrap().push(line);
            }
        });
        let mut client = Self {
            child,
            stdin,
            output,
            syncs: 0,
        };
        client.sync();
        client
    }

    /// Wait until tmux has answered a command sent now, so whatever it
    /// sent before has arrived.
    pub fn sync(&mut self) {
        self.syncs += 1;
        let marker = format!("sync-{}", self.syncs);
        writeln!(self.stdin, "display-message {marker}").unwrap();
        for _ in 0..500 {
            if self.messages_raw().contains(&marker) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "control client never saw {marker}: {:?}",
            self.output.lock().unwrap()
        );
    }

    fn messages_raw(&self) -> Vec<String> {
        self.output
            .lock()
            .unwrap()
            .iter()
            .filter_map(|l| l.strip_prefix("%message "))
            .map(str::to_string)
            .collect()
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
