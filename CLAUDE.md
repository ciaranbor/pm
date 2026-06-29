# pm — Development Guidelines

## What is this?

`pm` is a terminal-based project manager built around tmux sessions and git worktrees.

## Architecture

Rust CLI using clap (derive macros) — a thin dispatch over a few
well-separated layers. The module name almost always matches the command
(`commands/agent_fork.rs` ↔ `pm agent fork`), so navigate by the tree; what
follows is only what the tree *doesn't* tell you.

- **Layering** — `cli.rs`/`main.rs`/`dispatch.rs` parse and dispatch,
  `commands/` handlers orchestrate, and all shelling-out is funnelled through
  the `git/`, `tmux.rs`, `gh.rs` wrappers — never inline in a handler.
- **State** (`state/`, TOML) — `~/.config/pm/` is the global registry,
  `<project>/.pm/` is per-project state; config precedence is project >
  global > unlimited. `ProjectEntry` optionally records `repo_url`/`state_remote`
  for cross-machine restore.
- **Bundled assets** — `agents/`, `baseline/`, `workflows/`, `skills/` are
  embedded via `include_str!` and installed by `pm init`/`pm upgrade` under one
  of two policies: **Overwrite** (skills/agents/baseline — the bundle is
  authoritative) or **Preserve** (workflows, like `.pm/hooks/` — never clobber
  user edits).
- **Portability** — `path_utils.rs` swaps `~/` ↔ `$HOME` so registry state
  moves between machines.

The sections below document the design decisions you can't recover by reading
the tree — these are the invariants to preserve.

### Agents as long-running message processors

pm agents are never-idle message processors, not one-shot scripts. This
is implemented with a Claude Code **Stop hook** (`pm claude hooks stop`,
installed by `pm claude hooks install` into `main/.claude/settings.json`). The
hook blocks until the agent's inbox has unread messages, then returns:

```json
{"decision": "block", "reason": "You have new messages. Run `pm msg read` …"}
```

Claude Code delivers this as a continuation prompt. The agent reads the
message, processes it, the turn ends, and the hook fires again — blocking
until the next message arrives.

Exception: if the Stop event reports a running background task or active
cron and no messages are queued, the hook approves instead of blocking so
the running work isn't stalled. Recurring crons stay active between fires,
so an agent with one is message-delivered only at fire boundaries.

Initial context delivery differs by path:

- `pm feat new`/`feat adopt --workflow X` spawn the **whole `agents`
  team** (with or without `--context`); when `--context` is given, the
  brief is enqueued only to `brief_agents`. A context with an empty
  `brief_agents` is an error (nobody to brief).
- `pm agent spawn --context <x>` desugars to the same primitive as
  before: **enqueue a message, then spawn (or do nothing if already
  running).**
- `pm msg send <to> <body>` is a near-pure queue: it **never spawns a new
  agent**, errors when the recipient isn't a registered active agent, but
  **heals a dead tmux window of an active agent** (queues, then respawns
  only if the window is gone). This applies to cross-*scope* sends
  (`--scope`/`--upstream`, same project) too — they heal a dead window just
  like same-scope. Only cross-*project* sends truly never spawn — they assume
  the target agent in the foreign project already exists.

For spawn paths, the first turn is empty; the Stop hook blocks until the
queued message is available, then delivers it. The first-turn flow is
identical to every subsequent turn.

`pm msg reply` targets the last-read cross-scope message automatically — the
sender, scope, and project are recorded each time a message is read — so an
agent can reply without re-addressing.

`--context` (and `pr create/edit --body`) take a `-` sentinel meaning
"read the body from stdin", so long briefs can be fed via heredoc without
an approval prompt. (`--context` also accepts a literal string or a file
path; `agent spawn`'s is stdin-or-literal only.)

### Workflows vs agents

Two layers, deliberately decoupled:

- **Agent definitions** (`agents/<name>.md`) describe an agent's *job*:
  what they do and how they evaluate work. They ship with the
  `pm-workflow` skill but contain no routing prose.
- **Workflows** (`workflows/<name>/workflow.md`) define the per-feature
  *topology*: who hands off to whom, who reports back to the user. They
  live next to `config.toml` which declares `agents` (the full team pm
  spawns at `feat new --workflow X` time) and `brief_agents` (the subset
  that receives the `--context` brief).

This split lets the same agent (e.g. `implementer`) play different
routing roles in different features without forking the agent
definition. The `pm-workflow` skill is the bridge: every agent runs
`pm workflow show` at the start of every task to read the active
workflow's prose.

`pm feat new --context` *requires* `--workflow <name>`. A context with
no workflow has nobody to deliver it to. `WorkflowDef::validate` enforces the
contract: every team member must have a resolvable definition file (under
`main/.claude/agents/` or `~/.claude/agents/`), and `brief_agents ⊆` the team.

### Agent registry and the shared baseline

An agent registry entry's `active` flag is the single source of truth for its
lifecycle: `agent spawn` sets it true, `agent stop` false, and the
spawn/list/check/send paths read it. A separate `agent_definition` decouples the
registry key (display name / tmux window / `PM_AGENT_NAME`) from the
`claude --agent` definition, so several agents can run off one definition;
restart, fork, `pm open`, and the dead-window heal all preserve the alias.

The shared baseline is appended to every spawned agent's prompt via
`claude --append-system-prompt-file`, gated on the file existing at a single
spawn chokepoint (older projects without it spawn unchanged). Its content is
general to all agents and must **not** mention `.pm`. If a future `claude` drops
the flag the baseline would silently go dark, so pm probes `claude --help` at
spawn and `pm doctor` warns when the baseline is installed but unsupported.

### Information store vs messaging

Two different things, don't collapse them:

- **Information store** (`.pm/docs/`) is for **project-level persistent
  knowledge** — todos, issues, ideas, findings, and any other categories
  defined in `categories.toml`. The default set bootstrapped by `pm init`
  and `pm upgrade` is todo/issues/ideas/findings. Tracked by the `.pm/`
  state repo, managed by the orchestrator agent. Use `pm state push` to
  commit and push changes. The orchestrator deletes completed
  tasks/issues/ideas rather than marking them done (git history is the
  record), migrating any durable finding into `findings.md` first.
- **Messaging** (`pm msg`) is for **cross-scope or cross-role
  communication** — sending something to a *different* agent or a
  *different* scope. A queue, not a database.

Don't abuse messaging as persistent storage, and don't abuse the
information store as a mailbox.

### Orchestrator/feature boundary

`main` is a **dispatcher, not a relay**: it spins up features, then steps
back. By default feature agents own the feature and report to the user in
their own tmux session rather than messaging `main` (explicit instructions
can override). The standing feature→project channel is `summary.md`,
triaged by the orchestrator on cleanup (the automated "Feature 'X' was
cleaned up" message is the trigger); completion is the user's decision,
made by merging, so there's no agent-driven "done" status. The `feat new`
brief is delivered non-repliably — sent with sender `no-reply-brief` and no
scope, so `pm msg read` shows no `Reply:` hint and the agent has no `main`
reply target. The boundary itself now lives in the
baseline (positive "report to the user") and `main.md` (which owns
`../.pm/`); intra-feature handoffs stay as messaging, with routing prose in
`workflows/*/workflow.md`.

### Feature summary lifecycle

Each feature maintains a `summary.md` in its worktree root as a living
document, kept brief and high signal-to-noise — just what the
orchestrator needs to triage, plus succinct out-of-scope bugs/ideas.
Each workflow's `workflow.md` names the single agent who owns
`summary.md`, stated in that role's section; format/brevity guidance is
single-sourced in the `pm workflow show` command, which appends it to that
output (not duplicated in the agent defs, the `workflow.md` files, or the
`pm-workflow` skill body). On `feat delete`, `summary.md` is collected to
`.pm/summaries/<feature>.md` so the orchestrator can triage its contents into
project-level docs.

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
