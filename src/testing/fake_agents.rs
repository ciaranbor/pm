//! Agent windows running a harness stand-in, registered as pm would.

use super::TestServer;
use super::fake_harness::{fake_claude, fake_claude_at_prompt, fake_harness_binary};

impl TestServer {
    /// Create a tmux window running a stand-in `claude` (a renamed `sleep`)
    /// to simulate an agent mid-turn. Registers the agent in the registry
    /// and waits until the window reads as busy so callers can immediately
    /// query liveness. Returns the tmux window target.
    pub fn spawn_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        crate::tmux::send_line(
            self.name(),
            &target,
            &format!("{} 999", fake_claude().display()),
        )
        .unwrap();
        self.await_liveness(
            &target,
            agent_name,
            crate::commands::running_agents::Liveness::Busy,
        );
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    /// [`Self::spawn_fake_agent`] whose window shows Claude Code's input
    /// box, with the cursor on its prompt line so typed keys echo there.
    pub fn spawn_prompting_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let script = fake_claude_at_prompt();
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        crate::tmux::send_line(self.name(), &target, &format!("exec {}", script.display()))
            .unwrap();
        self.await_liveness(
            &target,
            agent_name,
            crate::commands::running_agents::Liveness::Busy,
        );
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    /// [`Self::spawn_fake_agent`] for an agent whose harness has exited: its
    /// window runs only the shell.
    pub fn spawn_dead_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    fn await_liveness(
        &self,
        target: &str,
        agent_name: &str,
        want: crate::commands::running_agents::Liveness,
    ) {
        self.await_harness_liveness(
            target,
            agent_name,
            crate::harness::Harness::ClaudeCode,
            want,
        );
    }

    fn await_harness_liveness(
        &self,
        target: &str,
        agent_name: &str,
        harness: crate::harness::Harness,
        want: crate::commands::running_agents::Liveness,
    ) {
        use crate::commands::running_agents::{Liveness, liveness};
        let config = crate::state::project::HarnessConfig::default();
        // `mark_idle` waits for an idle one's hook.
        if want == Liveness::Idle {
            return;
        }
        // Under heavy load (parallel tests) this can take longer than
        // usual, so we poll generously.
        for _ in 0..500 {
            if let Ok(processes) = crate::tmux::pane_processes(self.name(), target)
                && liveness(Some(&processes), harness, &config) == want
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for window '{agent_name}' ({target}) to read as {want:?}; \
             pane:\n{:?}",
            crate::tmux::capture_pane(self.name(), target),
        );
    }

    /// [`Self::spawn_fake_agent`] for an agent between turns: its pane runs
    /// a process whose command line carries pm's Stop hook.
    pub fn spawn_idle_fake_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        // The `; :` keeps `sh` from exec'ing `sleep` in its own place.
        crate::tmux::send_line(
            self.name(),
            &target,
            &format!(
                "exec sh -c 'sleep 999; :' {}",
                crate::commands::hooks_install::PM_HOOK_MARKER
            ),
        )
        .unwrap();
        self.await_liveness(
            &target,
            agent_name,
            crate::commands::running_agents::Liveness::Idle,
        );
        self.mark_idle(project_root, feature, agent_name, &target);
        self.register_fake_agent(project_root, feature, agent_name);
        target
    }

    /// Make the process in `target`'s pane that carries pm's Stop hook the
    /// agent's waiter, and mark the agent idle.
    fn mark_idle(
        &self,
        project_root: &std::path::Path,
        feature: &str,
        agent_name: &str,
        target: &str,
    ) {
        use crate::state::runtime::{self, Waiting, WaitingKind};
        for _ in 0..500 {
            let processes = crate::tmux::pane_processes(self.name(), target).unwrap_or_default();
            if let Some(hook) = processes.iter().find(|p| {
                p.command
                    .contains(crate::commands::hooks_install::PM_HOOK_MARKER)
            }) {
                runtime::take_waiter(project_root, feature, agent_name, hook.pid, None).unwrap();
                let idle = Waiting::now(WaitingKind::Idle, None);
                runtime::write_waiting(project_root, feature, agent_name, &idle).unwrap();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("no process in '{target}' carries pm's Stop hook");
    }

    /// An agent of `harness` whose pane runs `command` with `exec`, once the
    /// window reads as `want`. Pair it with [`fake_harness_binary`] to have
    /// the pane run the harness.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_harness_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
        harness: crate::harness::Harness,
        command: &str,
        want: crate::commands::running_agents::Liveness,
    ) -> String {
        let target = self.fake_agent_window(project_root, session_name, feature, agent_name);
        crate::tmux::send_line(self.name(), &target, &format!("exec {command}")).unwrap();
        self.await_harness_liveness(&target, agent_name, harness, want);
        if want == crate::commands::running_agents::Liveness::Idle {
            self.mark_idle(project_root, feature, agent_name, &target);
        }
        self.register_harness_agent(project_root, feature, agent_name, harness);
        target
    }

    /// An agent of `harness` whose pane shows an empty input box, codex's
    /// composer for codex and Claude Code's otherwise, and records every
    /// byte it is sent, waiting at that box. Returns its
    /// window and the file the bytes go to.
    pub fn spawn_recording_agent(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
        harness: crate::harness::Harness,
    ) -> (String, std::path::PathBuf) {
        let received = project_root.join(format!(".pm/received-{agent_name}"));
        let cat = fake_harness_binary(harness, std::path::Path::new("/bin/cat"));
        let script = project_root.join(format!(".pm/recorder-{agent_name}"));
        let (input_box, prompt) = match harness {
            crate::harness::Harness::Codex => ("\\n› \\n\\n  footer\\n\\033[3A", "›"),
            _ => ("──── agent ─\\n❯ \\n────────\\n\\033[2A", "❯"),
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nclear\nprintf '\\033[?2004h{input_box}\\033[3G'\n\
                 stty raw -echo\nexec {} -u > {}\n",
                cat.display(),
                received.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let target = self.spawn_harness_agent(
            project_root,
            session_name,
            feature,
            agent_name,
            harness,
            &script.display().to_string(),
            crate::commands::running_agents::Liveness::Busy,
        );
        self.wait_for_pane_text(&target, prompt);
        (target, received)
    }

    fn fake_agent_window(
        &self,
        project_root: &std::path::Path,
        session_name: &str,
        feature: &str,
        agent_name: &str,
    ) -> String {
        let target = crate::tmux::new_window(
            self.name(),
            session_name,
            &project_root.join(feature),
            Some(agent_name),
            true,
        )
        .unwrap();
        crate::tmux::mark_agent_pane(self.name(), &target).unwrap();
        target
    }

    fn register_fake_agent(&self, project_root: &std::path::Path, feature: &str, agent_name: &str) {
        self.register_harness_agent(
            project_root,
            feature,
            agent_name,
            crate::harness::Harness::ClaudeCode,
        );
    }

    fn register_harness_agent(
        &self,
        project_root: &std::path::Path,
        feature: &str,
        agent_name: &str,
        harness: crate::harness::Harness,
    ) {
        use crate::state::agent::{AgentEntry, AgentRegistry, AgentType};
        use crate::state::paths;

        let agents_dir = paths::agents_dir(project_root);
        let mut registry = AgentRegistry::load(&agents_dir, feature).unwrap();
        registry.register(
            agent_name,
            AgentEntry {
                agent_type: AgentType::Agent,
                session_id: String::new(),
                window_name: agent_name.to_string(),
                active: true,
                agent_definition: None,
                harness,
                spawned_at: None,
            },
        );
        registry.save(&agents_dir, feature).unwrap();
        // Launched with what is current, as a spawn would record.
        let config =
            crate::state::project::ProjectConfig::load(&paths::pm_dir(project_root)).unwrap();
        let global = crate::state::project::GlobalConfig::load_or_default();
        let entry = registry.get(agent_name).unwrap();
        let stamp = crate::commands::launch_stamp::current(
            project_root,
            feature,
            agent_name,
            entry,
            &config,
            &global,
        )
        .unwrap();
        crate::state::runtime::write_launch_stamp(project_root, feature, agent_name, &stamp)
            .unwrap();
    }
}
