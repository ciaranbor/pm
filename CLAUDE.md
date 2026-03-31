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
- `src/commands/` — one module per command group (project, feat, permissions, claude, etc.)

## Development

```sh
cargo build                    # build
cargo test                     # run all tests
cargo clippy                   # lint
cargo fmt                      # format
cargo run -- <args>            # run pm with arguments
```

Before completing any task, always run: `cargo fmt && cargo clippy && cargo test`

**Important:** Never run `cargo test` from within parallel subagents. Tests create real tmux sessions that consume ptys, and concurrent test runs can exhaust the macOS pty limit (511), freezing the system. Always run tests sequentially in the main agent.

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
