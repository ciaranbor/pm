---
name: pm-sandbox
description: Exercise a pm build by hand or end to end without touching the real environment. Use whenever you run, try, QA, or smoke-test pm changes. Never run a build under test against the real registry, home directory, or default tmux server.
---

# pm sandbox

- `scripts/sandbox` provides an isolated `$HOME`, a private tmux server, and
  the built `pm` plus recording `claude`/`codex` shims on `PATH`.
  `scripts/sandbox --help` is the reference. Usual loop: `up`,
  `run [-C DIR] -- pm …` (`DIR` is relative to the sandbox `$HOME`),
  `status`, `down`.
- Name your sandbox (`-n <feature>` or `PM_SANDBOX`): the default name is
  shared, so two agents using it collide. Always `down` when finished: a
  sandbox left up keeps its tmux server and ptys until someone removes it.
- `list` shows every sandbox on the machine with its creator and whether it
  is stale; `prune` downs the stale ones. `prune --all` also downs other
  agents' live sandboxes, so run it only when the user asks.
- `up` builds the working tree, and inside the sandbox `pm` is that build.
  Outside it, `pm` is the installed release and `cargo run --` the local
  build; both act on the real environment, so neither belongs in a QA run.
- Never select processes by pattern (`pgrep -f`, `pkill`, `killall`): a
  pattern matches the real agents' processes on the whole machine, their
  hooks included. Target pids under the sandbox, found through its tmux
  server (`list-panes -a -F '#{pane_pid}'` and their descendants), or let
  `down` stop them.
- `status` prints the harness invocations the shims recorded. Use
  `up --real` only when the behaviour under test needs a real harness.
- `PM_TMUX_SERVER` is what points pm at the private server, named
  `pm-test-<sandbox name>`. Pass `-L pm-test-<sandbox name>` on any hand-run
  `tmux` command.
- `tests/smoke.rs` holds end-to-end scenarios driven through the same
  sandbox (`cargo test --test smoke -- --ignored`); read it for how to drive
  a scenario, not as a substitute for exercising the change.
- Every tmux window holds a pty and test runs abort at 300 system-wide. A
  pty-budget failure means leaked sessions; recovery is in `AGENTS.md`
  (Development).
