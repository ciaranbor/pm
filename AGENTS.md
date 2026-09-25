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
  are overwritten, nothing is deleted.
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
- The hook yields (`{}`) only when the harness reports a running background
  task or active cron and nothing is queued, so the running work isn't
  stalled (its completion wakes the agent). Codex reports neither and has no
  second wake source, so codex agents block every turn.
- Hooks are installed once per machine for **every supported harness**, not
  only those in use; `pm doctor` checks only the harnesses the project's
  agents run on. The asymmetry is deliberate.
- Three context-delivery contracts: `feat new`/`feat adopt --workflow` spawn
  the whole team and brief only `brief_agents` (a context with none to brief
  is an error); `agent spawn --context` enqueues, then spawns or no-ops;
  `msg send` never spawns a new agent, errors when the recipient isn't
  active, and heals a dead window of an active one.

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
  (git history is the record); a durable finding is migrated to
  `findings.md` first.
- Messaging (`pm msg`) is cross-scope or cross-role communication: a queue,
  not a database. Don't use messaging as storage or the store as a mailbox.

### Orchestrator/feature boundary

- `main` is a dispatcher, not a relay: it starts features, then steps back.
- Feature agents report to the user in their own session, not to `main`.
- `summary.md` is the standing feature→project channel, triaged by the
  orchestrator on cleanup. Completion is the user's merge; there is no
  agent-driven "done" status.
- The `feat new` brief is non-repliable, so the agent has no `main` reply
  target.

### Lifecycle hooks

- The `PM_*` contract is per invocation, never session-scoped.
- A new variable must be meaningful (or documented empty) for all three hooks
  before it is added, and goes in README's table.

### Feature summary

- Each `workflow.md` names the single agent who owns `summary.md`.
- Content guidance is single-sourced in `pm workflow show`; never duplicate
  it into agent defs, workflow files, or the skill.

## Development

```sh
cargo build                    # build
cargo test                     # run all tests
cargo clippy                   # lint
cargo fmt                      # format
cargo run -- <args>            # test local changes (development only)
```

**Important:** Use `pm` (the installed binary) for pm commands; use `cargo
run --` only to test local, uncommitted changes.

Before completing any task, always run: `cargo fmt && cargo clippy && cargo test && cargo doc --no-deps` — the last must emit no warnings.

**Important:** Tests create real tmux sessions that consume ptys. A check in
`TestServer::new()` aborts the run once the system-wide pty count reaches 300
(macOS limit is 511); a pty-budget failure means leaked tmux sessions. Runs are
capped at 4 threads via `.cargo/config.toml`. Each test binary owns one
`pm-test-<pid>` tmux server with a `keepalive` session; dead-pid servers are
reaped at the next run's start and the current one is killed at exit. To
recover from a runaway run: `tmux -L pm-test-<pid> kill-server` (or `for s in
/tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename "$s") kill-server; rm -f
"$s"; done`). Always pass `-L` when touching a test server by hand.

### Sandbox and smoke tests

`scripts/sandbox` (`--help`) is a throwaway pm environment for trying changes
by hand or from an agent: its own `$HOME`, a private tmux server reached
through `PM_TMUX_SERVER` (the one production seam), and the built `pm` plus
recording `claude`/`codex` shims first on `PATH`. `tests/smoke.rs` runs the
built binary end to end in such a sandbox: `cargo test --test smoke --
--ignored`. Add a scenario only when the failure mode is environmental — cwd
or scope detection, the real config dir, inherited env, a command run from
inside the session it kills; never to mirror a lib test.

## Testing approach

TDD. Tests use real git repos and real tmux sessions, not mocks.

- Unit tests go in the same file as the code they test (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`
- Git tests create real repos in temp directories (`tempfile` crate)
- tmux tests use a dedicated test server (`tmux -L pm-test-<pid>`) to avoid interfering with the user's session
- Tests that don't need tmux use `setup_project_no_tmux` / `setup_project_with_feature_no_tmux` to avoid unnecessary pty allocation
- Always clean up tmux test sessions and temp dirs, even on test failure

## Code style

- Use `thiserror` for error types. Propagate errors with `?`, don't panic in library code.
- Keep modules focused. If a file grows past ~300 lines, split it.
- No unnecessary abstractions — three similar lines is better than a premature trait.
- External commands (git, tmux, gh) go through thin wrapper functions in `git/` / `tmux.rs` / `gh.rs`, not scattered throughout command handlers.
- All CLI commands and subcommands must support `--help` via clap derive.

## Documentation

When adding or changing commands/features, update:

- `README.md` — user-facing usage examples and command reference
- `AGENTS.md` — invariants and conventions only. Mechanism goes in the
  owning module's `//!`; user-facing behaviour in README. Keep this file
  under 200 lines.

## Commits

- Commit messages: imperative, concise, focused on "why"
- One logical change per commit
