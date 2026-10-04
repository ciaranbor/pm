# pm — Development Guidelines

## What is this?

`pm` is a terminal-based project manager built around tmux sessions and git
worktrees, with agents running inside them.

## Architecture

Rust CLI using clap (derive macros). The module name matches the command
(`commands/agent_fork.rs` ↔ `pm agent fork`), so navigate by the tree; module
docs (`//!`) hold each mechanism. What follows is only what the tree doesn't say.

- **Layering** — `cli.rs`/`main.rs`/`dispatch.rs` parse and dispatch,
  `commands/` handlers orchestrate, and all shelling-out is funnelled through
  the `git/`, `tmux.rs`, `gh.rs` wrappers — never inline in a handler.
- **State** (`state/`, TOML) — the pm config dir holds the global registry
  and global config; `<project>/.pm/` is per-project state. Config precedence
  is project > global > unset.
- **Bundled assets** (`commands/skills.rs`) — two tiers. Bundled skills,
  agent defs, workflows, and the baseline install into the **global tier**,
  where bundled names are reserved and rewritten on upgrade; the **project
  tier** holds only the user's customs and shadows the global tier by name
  — except Claude Code skills, where personal outranks project, so `pm
  doctor` reports the shadow instead. No harness reads pm's canonical store,
  so each tier is *projected* into the harness's layout: same-named files
  are overwritten; nothing is deleted but pm's abandoned temp files and a
  feature's projection of a skill its branch deleted (`commands/seed.rs`).
  Bundled workflows are never git-backed (`commands/state_gitignore.rs`).
- **Portability** — `path_utils.rs` swaps `~/` ↔ `$HOME` so registry state
  moves between machines.
- **Harness** (`harness/`) — the agent CLI pm launches, behind a `Harness`
  enum: each seam is a `match` in `harness/mod.rs`, never a trait, and
  harness-specific knowledge lives only in `harness/<name>.rs` (read its
  `//!` first). `agent_spawn` and the registry stay neutral. Model ids and
  permission modes are the harness's own vocabulary, passed through unvalidated.

## Invariants

Design decisions you can't recover by reading the tree. Preserve them.

### Agents are never-idle message processors

- An agent is a message processor, not a one-shot script: a Stop hook blocks
  until its inbox has unread messages, then returns a `block` decision the
  harness delivers as a continuation prompt. The first turn is identical to
  every later one — a spawn-time context only queues the first message.
- That prompt instructs a bare `pm msg read`, so a bare read must never
  error when several senders have unread messages: it takes the oldest
  sender's (README has the selection rule).
- The hook yields (`{}`) only for a reported running background task or
  active cron with nothing queued; its completion wakes the agent.
- Only the harness or a terminal ends the hook undecided — never pm state.
- A waiting marker only refines busy; a running Stop hook or dead harness wins.
- Hooks are installed once per machine for **every supported harness**;
  `pm doctor` checks only those the project's agents run on, deliberately.
- Hooks read the agent's identity from their environment, so a spawn never
  attaches to a shared harness server, whose hooks run in its own.
- opencode's Stop hook is a bundled plugin; a loop that stops itself must say so.
- An opencode agent never spawns without a model row and reaches only the
  providers pm config names; keys are named by env var, never stored.
- Three context-delivery contracts: `feat new`/`feat adopt --workflow` spawn
  the whole team (refused up front if a member's harness can't run it) and
  brief only `brief_agents` (none is an error); `agent spawn --context`
  enqueues, then spawns or no-ops — ungated, being also the heal path;
  `msg send` never spawns, errors on an inactive recipient, heals a dead
  window, re-arms an unarmed one only at an empty prompt.

### Workflows vs agents

- **Agent definitions** (`agents/<name>.md`) describe a job: what the agent
  does and how it evaluates work. No routing prose.
- **Workflows** (`workflows/<name>/workflow.md` + `config.toml`) define the
  per-feature topology: the team, who receives the brief, who hands off to
  whom, who reports to the user.
- One definition can play different routing roles in different features.
  The `pm-workflow` skill is the bridge: every agent runs `pm workflow show`
  at the start of every task.
- Definitions resolve only from the canonical `.agents/agents/` stores
  (project, then global). A harness's own dir is a projection, never a
  source — a def hand-written only in `.claude/agents/` does not resolve.
- The reserved name `default` means a definition-less vanilla session:
  validation skips it and the spawn passes no definition, unconditionally.

### Registry, config, and the baseline

- An agent's `active` flag is the single source of truth for its lifecycle.
- `agent_definition` decouples the registry key (display name, tmux window,
  `PM_AGENT_NAME`) from the definition launched; restart, fork, `pm open`,
  and the dead-window heal all preserve the alias.
- Per-agent `[agents.*]` settings are re-resolved from config at every spawn
  and never stored on the registry entry — that is what lets restart, fork,
  and heal pick up edits. There is deliberately no spawn-time override flag.
- The one stored setting is `harness`: a session id only means something to
  the harness that produced it, so when the configured harness no longer
  matches, a spawn starts fresh instead of resuming (and says so) and
  `agent fork` refuses.
- The shared baseline is general to all agents and must **not** mention
  `.pm`. It is appended through the harness's prompt mechanism, gated on the
  file existing, at the single spawn chokepoint.
- The notice board pushes standing directives *down* into agents. Agents
  report through `pm msg`, never by writing the board.

### Information store vs messaging

- The information store (`.pm/docs/`) is project-level persistent knowledge,
  managed by the orchestrator. Completed items are deleted, not marked done
  (git history is the record); a durable finding moves to `findings.md` first.
- Messaging (`pm msg`) is cross-scope or cross-role communication: a queue,
  not a database. Don't use messaging as storage or the store as a mailbox.

### Orchestrator/feature boundary

- `main` is a dispatcher, not a relay: it starts features, then steps back.
- Feature agents report to the user in their own session, not to `main`.
- The summary (`.pm/summaries/`, edited in place) is the feature→project
  channel, kept until `main` deletes it. `ready` asks `main` to review it
  for gaps; the always-sent merge/delete notice, naming which, is the sole
  triage trigger. `blocked` never messages `main`; nothing pm sends resets it.
- Team status and the PR-derived `status` are separate: sync can't clobber it.
- Each `workflow.md` names the single summary owner; content guidance is
  single-sourced in `pm workflow show`, never in defs, workflows, or skills.
- The `feat new` brief is non-repliable: the agent has no `main` reply target.

### Lifecycle hooks

- The `PM_*` contract is per invocation, never session-scoped.
- A new variable must be meaningful (or documented empty) for all three hooks
  before it is added, and goes in README's table.

## Development

```sh
cargo build                    # build
cargo test                     # run all tests
cargo clippy --all-targets     # lint, tests included
cargo fmt                      # format
cargo run -- <args>            # test local changes (development only)
android/gradlew -p android ktfmtFormat lint test assembleRelease  # when android/ changes; no cargo step builds it
```

**Important:** Use `pm` (the installed binary) for pm commands; use `cargo
run --` only to test local, uncommitted changes.

Before completing any task, always run:
`cargo fmt && cargo clippy --all-targets && cargo test && cargo doc --no-deps`
— the last must emit no warnings. `cargo test` needs `node` 22.18+
(opencode plugin).

**Important:** Tests create real tmux sessions that consume ptys. A check in
`TestServer::new()` aborts the run once the system-wide pty count reaches 300
(macOS limit is 511); a pty-budget failure means leaked tmux sessions. Runs are
capped at 4 threads via `.cargo/config.toml`. Each test binary owns one
`pm-test-<pid>` tmux server (no tmux config, `/bin/sh` windows, a `keepalive`
session); dead-pid servers are reaped at the next run's start and the current
one is killed at exit. To recover from a runaway run: `tmux -L pm-test-<pid>
kill-server` (or `for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename
"$s") kill-server; rm -f "$s"; done`). Always pass `-L` to a test server by hand.

### Sandbox and smoke tests

`scripts/sandbox` (`--help`) is a throwaway pm environment: its own `$HOME`,
a private tmux server reached through `PM_TMUX_SERVER` (the one production
seam), and the built `pm` plus recording harness shims first on `PATH`.
`tests/smoke.rs` runs the built binary in one: `cargo test --test smoke --
--ignored`. Add a scenario only when the failure mode is environmental — cwd
or scope detection, the real config dir, inherited env, a command run from
inside the session it kills; never to mirror a lib test. A change touching a
`Harness` enum seam verifies every supported harness live (`up --real`, set
up per `--help`), or records in the feature summary each one it could not.

## Testing approach

TDD. Tests use real git repos and real tmux sessions, not mocks.

- Unit tests go in the same file as the code they test (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`
- Git tests create real repos in temp directories (`tempfile` crate)
- Tests that don't need tmux use `setup_project_no_tmux` / `setup_project_with_feature_no_tmux` to avoid unnecessary pty allocation
- Always clean up tmux test sessions and temp dirs, even on test failure

## Code style

- Use `thiserror` for error types. Propagate errors with `?`, don't panic in library code.
- Keep modules focused. If a file grows past ~300 lines, split it.
- No unnecessary abstractions — three similar lines is better than a premature trait.
- All CLI commands and subcommands must support `--help` via clap derive.

## Documentation

When adding or changing commands/features, update:

- `README.md` — user-facing usage and concepts; flags belong in `--help`
- `AGENTS.md` — invariants and conventions only, under 200 lines. Mechanism
  goes in the owning module's `//!`; user-facing behaviour in README.

## Commits

- Commit messages: imperative, concise, focused on "why"
- One logical change per commit
