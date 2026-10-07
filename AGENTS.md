# pm — Development Guidelines

`pm` is a terminal-based project manager built around tmux sessions and git
worktrees, with agents running inside them.

## Architecture

Rust CLI using clap (derive macros). The module name matches the command
(`commands/agent_fork.rs` ↔ `pm agent fork`), so navigate by the tree; module
docs (`//!`) hold each mechanism. What follows is only what the tree doesn't say.

- **Layering** — `cli.rs`/`main.rs`/`dispatch/` parse and dispatch,
  `commands/` handlers orchestrate, and all shelling-out is funnelled through
  wrappers (`git/`, `tmux.rs`, `gh.rs`, `editor.rs`, …) — never inline in a handler.
- **State** (`state/`, TOML) — the pm config dir holds the global registry and
  config; `<project>/.pm/` is per-project state. Precedence: project > global > unset.
- **Bundled assets** (`bundled/`, embedded by `commands/skills/`) — a global
  tier, where bundled names are reserved, and a project tier of the user's
  customs that shadows it by name, each *projected* into the harness's layout.
  Projection deletes only what `skills/mod.rs`'s `//!` lists. Bundled workflows are
  never git-backed (`state_gitignore.rs`).
- **Portability** — `path_utils.rs` swaps `~/` ↔ `$HOME` so registry state moves.
- **Releases** — one version for pm and app, Cargo.toml's; one APK signing key, ever;
  a removed `pm serve` path joins `RETIRED` (`serve/routes.rs`) so old apps blame themselves.
- **Harness** (`harness/`) — the agent CLI pm launches, behind a `Harness` enum:
  each seam is a `match` on `Harness` in `harness/seams/`, never a trait, and
  harness-specific knowledge lives only in `harness/<name>.rs` (read its `//!`
  first). `agent_spawn` and the registry stay neutral. Model ids and permission
  modes are the harness's own vocabulary, passed through unvalidated.

## Invariants

Design decisions you can't recover by reading the tree. Preserve them.

### Agents are never-idle message processors

- After each turn pm's waiter waits on the agent's inbox, then wakes it with a
  continuation through the harness's own input path — never keystrokes. The
  first turn is like any other: a spawn-time context only queues a message.
- That prompt instructs a bare `pm msg read`, so a bare read must never
  error when several senders have unread messages: it takes the oldest
  sender's (README has the selection rule).
- Idle is the waiting marker plus a live waiter, never the marker alone
  (`state/runtime.rs`). A continuation reaching an empty inbox is dropped.
- Hooks install for **every supported harness** (`hooks_install.rs` says why)
  and read the agent's name and worktree from their env (cwd only as a fallback),
  so a spawn never attaches to a shared harness server, whose hooks run in its own.
- Every waiter has a breaker; a loop that stops itself must say so.
- An opencode agent never spawns without a model row and reaches only the
  providers pm config names; keys are named by env var, never stored.
- Context reaches agents three ways, each contract in its module's `//!`:
  a team brief (`feat_common`), `agent spawn --context` (the heal path), and
  `msg send` (`agent_send`), which never spawns. Remote input is typed like local input,
  never a message; a dialog is answered through its harness's hook, or as a
  prompt in its harness's own reply format where no hook holds it — never keys.

### Workflows vs agents

- **Agent definitions** (`agents/<name>.md`) describe a job: what the agent
  does and how it evaluates work. No routing prose.
- **Workflows** (`workflows/<name>/workflow.md` + `config.toml`) define the
  per-feature topology: the team, who receives the brief, who hands off to
  whom, who reports to the user.
- One definition can play different routing roles in different features;
  every agent runs `pm workflow show` (the `pm-workflow` skill) each task.
- Definitions resolve only from the canonical `.agents/agents/` stores
  (project, then global). A harness's own dir is a projection, never a
  source — a def hand-written only in `.claude/agents/` does not resolve.
- The reserved name `plain` means a definition-less vanilla session:
  validation skips it and the spawn passes no definition, unconditionally.

### Registry, config, and the baseline

- An agent's `active` flag is the single source of truth for its lifecycle.
- `agent_definition` decouples the registry key (display name, tmux window,
  `PM_AGENT_NAME`) from the definition launched; restart, fork, `pm open`,
  and the dead-window heal all preserve the alias.
- Per-agent `[agents.*]` settings are re-resolved from config at every spawn
  and never stored on the registry entry — that is what lets restart, fork,
  and heal pick up edits. There is deliberately no spawn-time override flag.
- The one stored setting is `harness`: a session id means something only to the
  harness that produced it, so on a mismatch a spawn starts fresh instead of
  resuming (and says so) and `agent fork` refuses.
- The shared baseline is general to all agents and must **not** mention `.pm`;
  it is appended through the harness's prompt mechanism, gated on the file
  existing, at the single spawn chokepoint.
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

**Important:** Tests create real tmux sessions that consume ptys; a
pty-budget abort means leaked sessions (`src/testing/` has the budget and
recovery). Always pass `-L` to a test server by hand.

### Sandbox and smoke tests

`scripts/sandbox` (`--help`) is a throwaway pm environment; `tests/smoke.rs`
runs the built binary in one and says what earns a scenario. A `Harness` seam
change verifies every harness live (`cargo test --test real_harness --
--ignored`, else `up --real`), or records in the summary each it could not.

## Testing approach

TDD. Tests use real git repos (in `tempfile` dirs) and real tmux sessions, not mocks.

- Unit tests go in the same file as the code they test (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`
- Tests that don't need tmux use `setup_project_no_tmux` / `setup_project_with_feature_no_tmux` to avoid unnecessary pty allocation
- Always clean up tmux test sessions and temp dirs, even on test failure

## Code style

- Use `thiserror` for error types. Propagate errors with `?`, don't panic in library code.
- Keep modules focused. If a file grows past ~300 lines, split it.
- No unnecessary abstractions — three similar lines is better than a premature trait.
- All CLI commands and subcommands must support `--help` via clap derive.

## Documentation

When adding or changing commands/features, update:

- `README.md` — usage and concepts; reference in `docs/`; flags in `--help`
- `AGENTS.md` — invariants and conventions only, under 200 lines. Mechanism
  goes in the owning module's `//!`; user-facing behaviour in README.

## Commits

- Commit messages: imperative, concise, focused on "why"
- One logical change per commit
