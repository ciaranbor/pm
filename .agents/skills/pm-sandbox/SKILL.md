---
name: pm-sandbox
description: Exercise a pm build by hand or end to end without touching the real environment. Use whenever you run, try, QA, or smoke-test pm changes, the Android app included. Never run a build under test against the real registry, home directory, or default tmux server.
---

# pm sandbox

- `scripts/sandbox` provides an isolated `$HOME`, a private tmux server, and
  the built `pm` plus recording `claude`/`codex`/`opencode` shims on `PATH`.
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
  `up --real` only when the behaviour under test needs a real harness; a
  `Harness` seam change needs every one (`AGENTS.md`, Sandbox and smoke
  tests).
  Claude Code there authenticates with the user's long-lived token, which
  `--real` loads from outside the sandbox (`--help` says where); never copy
  it into the sandbox or print it.
- `PM_TMUX_SERVER` is what points pm at the private server, named
  `pm-test-<sandbox name>`. Pass `-L pm-test-<sandbox name>` on any hand-run
  `tmux` command.
- `tests/smoke.rs` holds end-to-end scenarios driven through the same
  sandbox (`cargo test --test smoke -- --ignored`); read it for how to drive
  a scenario, not as a substitute for exercising the change.
- Every tmux window holds a pty and test runs abort at 300 system-wide. A
  pty-budget failure means leaked sessions; recovery is in `AGENTS.md`
  (Development).
- Testing the Android app: prefer the headless emulator, `scripts/emulator`
  (`--help`). It runs beside the user's phone, their installed app and their
  `pm serve` without touching any of them, and uses its own adb server: drive
  it through the script (`adb -- …`, or `eval "$(scripts/emulator env)"`),
  never with a bare `adb`. Usual loop: `up`, `install` the APK
  (`assembleGoogleDebug`), start `pm serve --port P` in your sandbox
  (`scripts/sandbox run -- sh -c 'pm serve --port P >serve.log 2>&1 &'`, P not
  7764, the real server's), `pair -s <sandbox> -p P`, then `tap`, `type`,
  `ui` and `screenshot`, and `down`. Name it as you name your sandbox (`-n`).
  Use the phone only for what an emulator lacks (real push, the camera, the
  user's own install), and only when the user asks.
- Testing on the Android phone (`adb`): run the session under
  `scripts/phone hold -- <command>` so the phone neither sleeps nor locks
  mid-test, and its setting is put back afterwards even on failure. To hold
  across several tool calls, `hold --pid $PPID` (see `--help`) and
  `release` the same pid when done. When it reports the phone locked, ask
  the user to unlock it once; never type or ask for a PIN.
