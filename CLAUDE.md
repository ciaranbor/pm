# pm — Development Guidelines

## What is this?

`pm` is a terminal-based project manager built around tmux sessions and git worktrees.

## Architecture

Rust CLI using clap (derive macros). The codebase is organized as:

- `src/main.rs` — entry point (parse args, run dispatch, handle errors)
- `src/cli.rs` — clap CLI definition (all derive structs and enums)
- `src/dispatch.rs` — command dispatch (`run()` function, scope helpers)
- `src/state/` — TOML state management (project entries, feature state, config). `ProjectEntry` has optional `repo_url` (project git origin) and `state_remote` (.pm/ repo remote) fields for cross-machine restore. `GlobalConfig` (`~/.config/pm/config.toml`) holds global defaults like `max_features`. Precedence: project-level `.pm/config.toml` > global > unlimited. `FeatureState` has an optional `workflow: Option<String>` referencing a workflow under `<project>/.pm/workflows/<name>/`. `AgentsConfig` no longer carries a `default` field — the agent team spawned at feature creation is driven by the workflow's `agents` list instead.
- `src/state/workflow.rs` — Workflow definition (`WorkflowDef`) loader and validator. Each workflow lives under `<project>/.pm/workflows/<name>/` as `config.toml` + `workflow.md`. `config.toml` carries `description`, an optional `when_to_use` hint (advisory metadata aimed at `main` for picking a workflow; rendered as a `use when:` line by `pm workflow list`; a custom workflow needn't provide one), `agents` (the full team, all spawned at feature creation), and `brief_agents` (the subset that receives the `--context` brief; serde-aliased to the legacy `auto_spawn` key for back-compat). `effective_team()` returns `agents`, or `brief_agents` when `agents` is empty (fallback for custom workflows). `WorkflowDef::load` parses the TOML; `validate` checks that every member of the effective team has a definition file resolvable from `main/.claude/agents/` or `~/.claude/agents/` (feature worktree not consulted — it usually doesn't exist yet at `feat new` time) and that `brief_agents ⊆` the team.
- `src/git/` — git operations, split into submodules: `init.rs`, `branch.rs`, `worktree.rs`, `remote.rs`, `status.rs`
- `src/tmux.rs` — tmux operations (session create/kill/switch, display-menu)
- `src/gh.rs` — GitHub CLI wrapper (PR creation, editing, status queries via `gh`)
- `src/hooks.rs` — lifecycle hooks (post-create, post-merge, restore)
- `src/error.rs` — error types (`PmError` enum, `thiserror`)
- `src/testing.rs` — test utilities (shared tmux test server, RAII cleanup, no-tmux project setup helpers)
- `src/path_utils.rs` — portable path conversion (`~/` ↔ `$HOME`) for registry entries
- `src/messages/` — file-based message queue (send, read_at, next, list, wait, name validation). Supports cross-scope messaging: `send_with_scope` records the sender's scope in metadata, and `pm msg send --scope <name>` / `--upstream` deliver to a different feature's inbox. `pm msg reply` auto-routes replies using `.last_read` metadata (sender, scope, project) written by `agent_read` on each cursor advance. Split into `mod.rs` (core ops, path helpers, last-read persistence, tests), `types.rs` (`LastRead`, `MessageMeta`, etc.), `validation.rs`, and `cursor.rs`.
- `src/state/agent.rs` — per-feature agent registry (TOML state for spawned agents). `AgentEntry` has an `active: bool` flag that is the single source of truth for agent lifecycle state: set `true` by `agent spawn`, set `false` by `agent stop`, read by `agent_spawn_all`/`list`/`check`/`send` to determine whether an agent should be running. `AgentEntry` also has an optional `agent_definition: Option<String>` that decouples the registry key (display name / window / `PM_AGENT_NAME`) from the claude agent definition passed to `claude --agent`. `None` means the registry key doubles as the definition (back-compat); `Some(def)` is set when `pm agent spawn <name> --agent <def>` was used. `effective_definition(key)` resolves to `def` or falls back to `key`. Restart, fork, `agent_spawn_all`/`pm open`, and the `agent_send` dead-window heal all preserve the alias by reading from the registry.
- `src/commands/` — one module per command group (project, feat, claude, agent, msg, hooks_install, etc.). `feat_pr.rs` handles `pm feat pr create`, `feat_pr_edit.rs` handles `pm feat pr edit`.
- `src/commands/init.rs` — `pm init` with optional `--git <url>` for cloning; auto-detects default branch from remote
- `src/commands/open.rs` — reopens project sessions; before recreating, runs `doctor::diagnose` (with PR-state checks disabled to avoid network calls) and warns about non-recoverable drift; after recreating missing tmux sessions, respawns agents with `active = true` via `agent_spawn_all`. `OpenResult` carries the `main_session` name; the `pm open` dispatch handler then connects the user to it via `tmux::connect_session` (switch-client when inside tmux per `$TMUX`, attach-session otherwise). The attach step runs only in the real dispatch path (tests call `open()` directly with a test server and never attach).
- `src/commands/close.rs` — `pm close` kills all tmux sessions for a project without deleting state (counterpart to `pm open`). `pm close --all` sweeps every project in the global registry, reusing the single-project `close()` per project; missing dirs are skipped and per-project failures are reported without aborting the sweep (`--all` rather than `--global` because `pm state`'s `--global` means "the global registry repo", a different sense; `--all` reads as "all projects").
- `src/commands/hooks_install.rs` — installs the pm Stop hook into `main/.claude/settings.json`; see below
- `src/commands/agent_stop.rs` — `pm agent stop` (kill window, set `active = false`); accepts multiple names
- `src/commands/agent_delete.rs` — `pm agent delete` (kill window, remove registry entry entirely, wipe agent inbox via `messages::delete_inbox`); accepts multiple names. Terminal counterpart to `agent stop`: gone for good, no respawn, no inherited cursors/messages
- `src/commands/agent_restart.rs` — `pm agent restart` (kill window then respawn, preserving `active = true` and session for `--resume`); accepts multiple names
- `src/commands/agent_fork.rs` — `pm agent fork <source> <new-name>` spawns a new agent that starts with a copy of the source's conversation history. Implemented via Claude Code's built-in `claude --resume <source.session_id> --fork-session`, which loads the source's transcript but assigns a fresh session id, leaving the source's session file untouched. `SpawnClaudeParams` carries a `fork_session: bool` so other callers default to `false`.
- `agents/` — bundled agent definitions (reviewer, implementer, researcher, main), embedded via `include_str!`. Agent defs carry no `tools:` allowlist — on the `claude --agent` path each agent inherits all Claude Code tools (incl. `Skill`, so the bundled skills load). **Job-duty prose only** — cross-cutting operating rules (brevity, environment/CWD, messaging heredoc, `pm workflow show`, what "the user" means) live in the shared baseline (see `baseline/` below), not repeated per def. The reviewer keeps a role-specific scoped diff-inspection convention; `main.md` owns the `../.pm/` store boundary and dispatcher framing. `summary.md` ownership is named per-role in `workflows/*/workflow.md`, with format guidance in the `pm-workflow` skill — not in the agent defs. Routing topology is owned by the workflow (see below).
- `baseline/` — single bundled `pm-baseline.md` (the shared "operating baseline"), embedded via `include_str!`. `pm init`/`pm upgrade` install it to `main/.claude/pm-baseline.md` (**Overwrite** policy). Every agent pm spawns — including `main` — is launched with `claude --append-system-prompt-file <abs path>`, appended at the single `build_claude_cmd` chokepoint in `agent_spawn.rs` and **only when the file exists** (back-compat: older projects without it spawn unchanged). The content is general/valid for all agents and must **not** mention `.pm`. `skills::baseline_path` resolves the install path; `baseline_append_arg` gates the flag on existence. Regression guard: if a future `claude` drops the flag the baseline would silently go dark, so `agent_spawn::claude_supports_append_file` probes `claude --help` (tolerant of the bracket-collapsed `--append-system-prompt[-file]` form it actually prints) and `pm doctor` warns when the baseline is installed but the flag is unsupported.
- `workflows/` — bundled workflow definitions (implement-and-review, research-implement-review, research-only, pr-review), each a `<name>/{config.toml,workflow.md}` pair embedded via `include_str!`. `pm init`/`pm upgrade` install them into `<project>/.pm/workflows/`. Workflows use a "Preserve" install policy: missing workflows are installed but user-modified ones are never overwritten (same spirit as `.pm/hooks/`). Skills, agents, and the baseline use "Overwrite" (the bundle is authoritative). The shared `BundledItem` system in `src/commands/skills.rs` handles all four kinds (`BundledKind::{Skill,Agent,Baseline,Workflow}`).
- `src/commands/claude_export.rs` — `pm claude export` tars Claude session data with a manifest for cross-machine transfer
- `src/commands/claude_import.rs` — `pm claude import` extracts tarball, resolves local paths from registry, rewrites embedded paths
- `src/commands/summary.rs` — `pm summary write` writes/overwrites `.pm/summaries/<feature>.md`
- `src/commands/workflow.rs` — `pm workflow show` (prints active workflow's `workflow.md`) and `pm workflow list` (lists installed workflows with descriptions). Used by the bundled `pm-workflow` skill so agents can discover their per-feature routing at the start of every task.
- `src/commands/docs.rs` — information store management (`bootstrap`, submodule migration)
- `src/commands/state_cmd.rs` — git-backed state backup and sync (`init`, `remote`, `push`, `pull`, `status`, `backfill`). Supports both per-project `.pm/` and global registry `~/.config/pm/` via `--global` flag. Shared `RepoContext` eliminates duplication between the two modes. `backfill` reads origin URLs from existing projects and writes `repo_url`/`state_remote` into the global registry.
- `src/commands/restore.rs` — `pm restore` rebuilds all projects on a fresh machine from the global registry, cloning repos (`repo_url`), pulling `.pm/` state (`state_remote`), recreating missing feature worktrees, and opening tmux sessions.
- `src/commands/self_update.rs` — `pm self-update` pulls latest pm source (ff-only), rebuilds via `cargo install`, warns about active features, then runs `upgrade --all`. Finds pm's own source via the global registry lookup for project "pm".
- `skills/` — bundled skill definitions (pm, messaging, pm-workflow), embedded via `include_str!`. Skills are installed at project level (`main/.claude/skills/`) and reach every agent via on-demand auto-invoke. `pm-workflow` teaches agents to run `pm workflow show` to discover per-feature routing. The `messaging` skill prescribes the heredoc-redirect send form (`pm msg send <agent> <<'EOF' … EOF`) for multi-line/markdown bodies.

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
  **heals a dead tmux window of an active agent** (queues, then calls
  `agent_spawn` which is a no-op if the window is alive). This applies to
  cross-*scope* sends (`--scope`/`--upstream`, same project) too — they go
  through the same `agent_send` path and heal a dead window just like
  same-scope. Only cross-*project* sends (`agent_send_cross_project`, a
  separate function) truly never spawn — they assume the target agent in
  the foreign project already exists.

For spawn paths, the first turn is empty; the Stop hook blocks until the
queued message is available, then delivers it. The first-turn flow is
identical to every subsequent turn.

`--context` (and `pr create/edit --body`) take a `-` sentinel meaning
"read the body from stdin", so long briefs can be fed via heredoc without
an approval prompt. Resolved in `feat_new::resolve_context` (shared, also
does file/literal) and `feat_new::resolve_stdin_context` (stdin-only, for
`agent spawn`).

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
no workflow has nobody to deliver it to.

### Information store vs messaging

Two different things, don't collapse them:

- **Information store** (`.pm/docs/`) is for **project-level persistent
  knowledge** — todos, issues, ideas, findings, and any other categories
  defined in `categories.toml`. The default set bootstrapped by `pm init`
  and `pm upgrade` is todo/issues/ideas/findings (the hardcoded
  `DEFAULT_CATEGORIES` in `src/commands/docs.rs`). Tracked by the `.pm/`
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
brief is delivered non-repliably — `enqueue_initial_context`
(`feat_common.rs`) sends it via `messages::send` with sender
`no-reply-brief` and no scope, so `pm msg read` shows no `Reply:` hint and
the agent has no `main` reply target. The boundary itself now lives in the
baseline (positive "report to the user") and `main.md` (which owns
`../.pm/`); intra-feature handoffs stay as messaging, with routing prose in
`workflows/*/workflow.md`.

### Feature summary lifecycle

Each feature maintains a `summary.md` in its worktree root as a living
document, kept brief and high signal-to-noise — just what the
orchestrator needs to triage, plus succinct out-of-scope bugs/ideas.
Each workflow's `workflow.md` names the single agent who owns
`summary.md`, stated in that role's section; format/brevity guidance
lives in the `pm-workflow` skill (not in the agent defs). On `feat
delete`, `summary.md` is collected to `.pm/summaries/<feature>.md` so the
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
