# pm — Development Guidelines

## What is this?

`pm` is a terminal-based project manager built around tmux sessions and git worktrees.

## Architecture

Rust CLI using clap (derive macros). The codebase is organized as:

- `src/main.rs` — entry point, clap CLI definition, command dispatch
- `src/state/` — TOML state management (project entries, feature state, config)
- `src/git.rs` — git operations (branch, worktree, status checks)
- `src/tmux.rs` — tmux operations (session create/kill/switch, display-menu)
- `src/gh.rs` — GitHub CLI wrapper (PR creation, status queries via `gh`)
- `src/hooks.rs` — lifecycle hooks (post-create, post-merge, restore)
- `src/error.rs` — error types (`PmError` enum, `thiserror`)
- `src/testing.rs` — test utilities (shared tmux test server, RAII cleanup)
- `src/messages.rs` — file-based message queue (send, read_at, next, list, wait, name validation). Supports cross-scope messaging: `send_with_scope` records the sender's scope in metadata, and `pm msg send --scope <name>` / `--upstream` deliver to a different feature's inbox.
- `src/state/agent.rs` — per-feature agent registry (TOML state for spawned agents)
- `src/commands/` — one module per command group (project, feat, claude, agent, msg, hooks_install, etc.)
- `src/commands/hooks_install.rs` — installs the pm Stop hook into `main/.claude/settings.json`; see below
- `src/commands/agent_check.rs` — assembles checklists from agent definition frontmatter + project-specific files, sends as message
- `agents/` — bundled agent definitions (reviewer, implementer, researcher), embedded via `include_str!`. Frontmatter supports a `checklist:` field (YAML list of items for `pm agent check`)
- `src/commands/summary.rs` — `pm summary write` writes/overwrites `.pm/summaries/<feature>.md`
- `skills/` — bundled skill definitions (pm), embedded via `include_str!`

### Agents as long-running message processors

pm agents are never-idle message processors, not one-shot scripts. This
is implemented with a Claude Code **Stop hook** (`pm claude hooks stop`,
installed by `pm claude hooks install` into `main/.claude/settings.json`). The
hook blocks until messages are available by calling `agent_wait`
directly, then returns:

```json
{"decision": "block", "reason": "You have new messages. Run `pm msg read` …"}
```

Claude Code delivers this as a continuation prompt. The agent reads the
message, processes it, the turn ends, and the hook fires again — blocking
until the next message arrives.

Initial context (`pm feat new --context <x>`, `pm agent spawn --context
<x>`, `pm msg send <to> <body>` auto-spawn) all desugar to the same
primitive: **enqueue a message, then spawn (or do nothing if already
running).** The first turn is empty; the Stop hook blocks until the
queued message is available, then delivers it. The first-turn flow is
identical to every subsequent turn.

### Own-scope notes vs cross-scope messaging

Two different things, don't collapse them:

- **Information store** (future `pm note` / scratch state) is for
  **own-scope jotting** — notes, running context, TODOs that belong to
  the agent itself. Private. Persistent by default.
- **Messaging** (`pm msg`) is for **cross-scope or cross-role
  communication** — sending something to a *different* agent or a
  *different* scope. A queue, not a database.

Don't abuse messaging as persistent storage, and don't abuse notes as a
mailbox.

### Feature summary lifecycle

Each feature maintains a `summary.md` in its worktree root as a living
document. Agents update it throughout the feature lifecycle (the
researcher seeds it, the implementer maintains it). On `feat delete`,
`summary.md` is collected to `.pm/summaries/<feature>.md` so the
orchestrator can triage its contents into project-level docs.

## Development

```sh
cargo build                    # build
cargo test                     # run all tests
cargo clippy                   # lint
cargo fmt                      # format
cargo run -- <args>            # test local changes (development only)
```

**Important:** Use `pm` (the installed binary) to run pm commands in normal
usage. Only use `cargo run --` when you need to test local, uncommitted
source changes during pm development.

Before completing any task, always run: `cargo fmt && cargo clippy && cargo test`

**Important:** Never run `cargo test` from within parallel subagents. Tests create real tmux sessions that consume ptys, and concurrent test runs can exhaust the macOS pty limit (511), freezing the system. Always run tests sequentially in the main agent.

`cargo test` runs are capped at 4 threads via `.cargo/config.toml` (`RUST_TEST_THREADS=4`) to keep peak pty usage well under the macOS limit. Each test binary owns one `pm-test-<pid>` tmux server with a `keepalive` session; dead-pid servers from prior runs are reaped at startup of the next run, and the current run's server is killed via a `libc::atexit` handler on exit. If you ever need to manually recover from a runaway test run: `tmux -L pm-test-<pid> kill-server` (or `for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename "$s") kill-server; rm -f "$s"; done`).

## Testing approach

TDD. Tests use real git repos and real tmux sessions, not mocks.

- Unit tests go in the same file as the code they test (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`
- Git tests create real repos in temp directories (`tempfile` crate)
- tmux tests use a dedicated test server (`tmux -L pm-test`) to avoid interfering with the user's session
- Always clean up tmux test sessions and temp dirs, even on test failure

## Code style

- Use `thiserror` for error types. Propagate errors with `?`, don't panic in library code.
- Keep modules focused. If a file grows past ~300 lines, split it.
- No unnecessary abstractions — three similar lines is better than a premature trait.
- External commands (git, tmux, gh) go through thin wrapper functions in `git.rs` / `tmux.rs` / `gh.rs`, not scattered throughout command handlers.
- All CLI commands and subcommands must support `--help` via clap derive.

## Documentation

When adding or changing commands/features, update:

- `README.md` — user-facing usage examples and command reference
- `CLAUDE.md` — architecture, development guidelines, and any new conventions

## Commits

- Commit messages: imperative, concise, focused on "why"
- One logical change per commit
