//! Processes as `ps` sees them: what runs in a pane, and whether it is
//! still the same process.

use std::process::Command;

use crate::error::Result;

/// A process as `ps` saw it. The start time tells a pid's process apart
/// from a later one the pid was reused for.
#[derive(Debug, Clone)]
pub struct Process {
    pub pid: u32,
    started: String,
    /// The command line, its arguments separated by single spaces.
    pub command: String,
    /// Whether it is in its terminal's foreground process group: for a
    /// pane's shell, whether it is at its prompt rather than running a job.
    pub foreground: bool,
    /// Whether it was asleep: for a pane's shell at its prompt with no job
    /// in the foreground, whether it is waiting for input.
    pub asleep: bool,
}

/// A process can rewrite its own command line, so it is not part of what
/// makes two sightings the same process.
impl PartialEq for Process {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid && self.started == other.started
    }
}

impl Eq for Process {}

#[cfg(test)]
impl Process {
    pub fn new_for_test(pid: u32, command: &str) -> Self {
        Self {
            pid,
            started: String::new(),
            command: command.to_string(),
            foreground: true,
            asleep: false,
        }
    }
}

/// Every process on the machine, read once so that several panes can be
/// walked against one `ps` run.
pub struct ProcessTable(Vec<(Process, u32)>);

impl ProcessTable {
    pub fn read() -> Result<Self> {
        let table = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,pgid=,tpgid=,stat=,lstart=,command="])
            .output()?;
        Ok(Self(
            String::from_utf8_lossy(&table.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    let pid = fields.next()?.parse().ok()?;
                    let ppid = fields.next()?.parse().ok()?;
                    let pgid: i64 = fields.next()?.parse().ok()?;
                    let tpgid: i64 = fields.next()?.parse().ok()?;
                    // `S` sleeping, `I` idle (macOS: asleep over 20 s).
                    let asleep = fields.next()?.starts_with(['S', 'I']);
                    // `lstart` is always five fields: `Wed Oct  1 16:47:56 2026`.
                    let started: Vec<&str> = fields.by_ref().take(5).collect();
                    if started.len() < 5 {
                        return None;
                    }
                    let process = Process {
                        pid,
                        started: started.join(" "),
                        command: fields.collect::<Vec<_>>().join(" "),
                        foreground: pgid == tpgid,
                        asleep,
                    };
                    Some((process, ppid))
                })
                .collect(),
        ))
    }

    /// The process `root` and its descendants.
    pub fn tree(&self, root: u32) -> Vec<Process> {
        let mut found: Vec<Process> = self
            .0
            .iter()
            .filter(|(process, _)| process.pid == root)
            .map(|(process, _)| process.clone())
            .collect();
        let mut at = 0;
        while at < found.len() {
            let parent = found[at].pid;
            for (process, ppid) in &self.0 {
                if *ppid == parent && !found.contains(process) {
                    found.push(process.clone());
                }
            }
            at += 1;
        }
        found
    }
}
