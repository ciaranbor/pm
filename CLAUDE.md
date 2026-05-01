# pm — Development Guidelines

## What is this?

`pm` is a terminal-based project manager built around tmux sessions and git worktrees.

## Architecture

Rust CLI using clap (derive macros). The codebase is organized as:

- `src/main.rs` — entry point (parse args, run dispatch, handle errors)
- `src/cli.rs` — clap CLI definition (all derive structs and enums)
- `src/dispatch.rs` — command dispatch (`run()` function, scope helpers)
- `src/state/` — TOML state management (project entries, feature state, config). `ProjectEntry` has optional `repo_url` (project git origin) and `state_remote` (.pm/ repo remote) fields for cross-machine restore. `GlobalConfig` (`~/.config/pm/config.toml`) holds global defaults like `max_features`. Precedence: project-level `.pm/config.toml` > global > unlimited.
- `src/git/` — git operations, split into submodules: `init.rs`, `branch.rs`, `worktree.rs`, `remote.rs`, `status.rs`
- `src/tmux.rs` — tmux operations (session create/kill/switch, display-menu)
- `src/gh.rs` — GitHub CLI wrapper (PR creation, editing, status queries via `gh`)
- `src/hooks.rs` — lifecycle hooks (post-create, post-merge, restore)
- `src/error.rs` — error types (`PmError` enum, `thiserror`)
- `src/testing.rs` — test utilities (shared tmux test server, RAII cleanup, no-tmux project setup helpers)
- `src/path_utils.rs` — portable path conversion (`~/` ↔ `$HOME`) for registry entries
- `src/messages/` — file-based message queue (send, read_at, next, list, wait, name validation). Supports cross-scope messaging: `send_with_scope` records the sender's scope in metadata, and `pm msg send --scope <name>` / `--upstream` deliver to a different feature's inbox. `pm msg reply` auto-routes replies using `.last_read` metadata (sender, scope, project) written by `agent_read` on each cursor advance. Split into `mod.rs` (core ops, path helpers, last-read persistence, tests), `types.rs` (`LastRead`, `MessageMeta`, etc.), `validation.rs`, and `cursor.rs`.
- `src/state/agent.rs` — per-feature agent registry (TOML state for spawned agents). `AgentEntry` has an `active: bool` flag that is the single source of truth for agent lifecycle state: set `true` by `agent spawn`, set `false` by `agent stop`, read by `agent_spawn_all`/`list`/`check`/`send` to determine whether an agent should be running. `AgentEntry` also has an optional `agent_definition: Option<String>` that decouples the registry key (display name / window / `PM_AGENT_NAME`) from the claude agent definition passed to `claude --agent`. `None` means the registry key doubles as the definition (back-compat); `Some(def)` is set when `pm agent spawn <name> --agent <def>` was used. `effective_definition(key)` resolves to `def` or falls back to `key`. Restart, fork, auto-spawn, and `pm open` all preserve the alias by reading from the registry.
- `src/commands/` — one module per command group (project, feat, claude, agent, msg, hooks_install, etc.). `feat_pr.rs` handles `pm feat pr create`, `feat_pr_edit.rs` handles `pm feat pr edit`.
- `src/commands/init.rs` — `pm init` with optional `--git <url>` for cloning; auto-detects default branch from remote
- `src/commands/open.rs` — reopens project sessions; before recreating, runs `doctor::diagnose` (with PR-state checks disabled to avoid network calls) and warns about non-recoverable drift; after recreating missing tmux sessions, respawns agents with `active = true` via `agent_spawn_all`
- `src/commands/close.rs` — `pm close` kills all tmux sessions for a project without deleting state (counterpart to `pm open`)
- `src/commands/hooks_install.rs` — installs the pm Stop hook into `main/.claude/settings.json`; see below
- `src/commands/agent_stop.rs` — `pm agent stop` (kill window, set `active = false`); accepts multiple names
- `src/commands/agent_delete.rs` — `pm agent delete` (kill window, remove registry entry entirely, wipe agent inbox via `messages::delete_inbox`); accepts multiple names. Terminal counterpart to `agent stop`: gone for good, no respawn, no inherited cursors/messages
- `src/commands/agent_restart.rs` — `pm agent restart` (kill window then respawn, preserving `active = true` and session for `--resume`); accepts multiple names
- `src/commands/agent_check.rs` — assembles checklists from agent definition frontmatter + project-specific files, sends as message
- `src/commands/agent_fork.rs` — `pm agent fork <source> <new-name>` spawns a new agent that starts with a copy of the source's conversation history. Implemented via Claude Code's built-in `claude --resume <source.session_id> --fork-session`, which loads the source's transcript but assigns a fresh session id, leaving the source's session file untouched. `SpawnClaudeParams` carries a `fork_session: bool` so other callers default to `false`.
- `agents/` — bundled agent definitions (reviewer, implementer, researcher), embedded via `include_str!`. Frontmatter supports a `checklist:` field (YAML list of items for `pm agent check`)
- `src/commands/claude_export.rs` — `pm claude export` tars Claude session data with a manifest for cross-machine transfer
- `src/commands/claude_import.rs` — `pm claude import` extracts tarball, resolves local paths from registry, rewrites embedded paths
- `src/commands/summary.rs` — `pm summary write` writes/overwrites `.pm/summaries/<feature>.md`
- `src/commands/docs.rs` — information store management (`bootstrap`, submodule migration)
- `src/commands/state_cmd.rs` — git-backed state backup and sync (`init`, `remote`, `push`, `pull`, `status`, `backfill`). Supports both per-project `.pm/` and global registry `~/.config/pm/` via `--global` flag. Shared `RepoContext` eliminates duplication between the two modes. `backfill` reads origin URLs from existing projects and writes `repo_url`/`state_remote` into the global registry.
- `src/commands/restore.rs` — `pm restore` rebuilds all projects on a fresh machine from the global registry, cloning repos (`repo_url`), pulling `.pm/` state (`state_remote`), recreating missing feature worktrees, and opening tmux sessions.
- `src/commands/self_update.rs` — `pm self-update` pulls latest pm source (ff-only), rebuilds via `cargo install`, warns about active features, then runs `upgrade --all`. Finds pm's own source via the global registry lookup for project "pm".
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

### Information store vs messaging

Two different things, don't collapse them:

- **Information store** (`.pm/docs/`) is for **project-level persistent
  knowledge** — todos, issues, ideas, and any other categories defined in
  `categories.toml`. Tracked by the `.pm/` state repo, managed by the
  orchestrator agent. Use `pm state push` to commit and push changes.
  Bootstrapped by `pm init` and `pm upgrade`.
- **Messaging** (`pm msg`) is for **cross-scope or cross-role
  communication** — sending something to a *different* agent or a
  *different* scope. A queue, not a database.

Don't abuse messaging as persistent storage, and don't abuse the
information store as a mailbox.

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

**Important:** Tests create real tmux sessions that consume ptys. A safety check in `TestServer::new()` aborts the test run if system-wide pty count reaches 300 (macOS limit is 511). If tests fail with a pty budget message, check for leaked tmux sessions.

`cargo test` runs are capped at 4 threads via `.cargo/config.toml` (`RUST_TEST_THREADS=4`) to keep peak pty usage well under the macOS limit. Each test binary owns one `pm-test-<pid>` tmux server with a `keepalive` session; dead-pid servers from prior runs are reaped at startup of the next run, and the current run's server is killed via a `libc::atexit` handler on exit. If you ever need to manually recover from a runaway test run: `tmux -L pm-test-<pid> kill-server` (or `for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename "$s") kill-server; rm -f "$s"; done`).

## Testing approach

TDD. Tests use real git repos and real tmux sessions, not mocks.

- Unit tests go in the same file as the code they test (`#[cfg(test)] mod tests`)
- Integration tests go in `tests/`
- Git tests create real repos in temp directories (`tempfile` crate)
- tmux tests use a dedicated test server (`tmux -L pm-test`) to avoid interfering with the user's session
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
- `CLAUDE.md` — architecture, development guidelines, and any new conventions

## Commits

- Commit messages: imperative, concise, focused on "why"
- One logical change per commit
