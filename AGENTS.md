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
- **State** (`state/`, TOML) — the pm config dir (`dirs::config_dir()/pm`:
  `~/.config/pm` on Linux, `~/Library/Application Support/pm` on macOS) holds
  the global registry, `<project>/.pm/` is per-project state; config precedence is project >
  global > unset (unlimited features, no spawn flag). `ProjectEntry` optionally
  records `repo_url`/`state_remote` for cross-machine restore.
- **Bundled assets** — `agents/`, `baseline/`, `workflows/`, `skills/` are
  embedded via `include_str!` and installed by `pm init`/`pm upgrade` into the
  **global tier**, where the bundle is authoritative (bundled names are
  reserved and rewritten on upgrade): `~/.agents/{skills,agents}` +
  `~/.agents/pm-baseline.md` (`.agents/skills` is the cross-harness
  convention, `.agents/agents` is pm's own) and `<config dir>/workflows/`
  (the git-backed global analogue of `.pm/workflows/`). The **project tier**
  — `main/.agents/{skills,agents}`, `.pm/workflows/` — holds only the user's
  customs and shadows the global tier **by name**; there is no third
  "bundled" rank. (`.pm/hooks/` is the one seeded surface that is
  **preserved** — `hooks::bootstrap` only writes missing scripts, since those
  are user scripts, not a bundled item.) Agent defs resolve through
  `workflow::definition_paths`,
  workflows through `workflow::resolve_dir` (which also reports the `Tier`);
  the baseline is global-only (`skills::baseline_path`, with a read-only
  fallback to the project/legacy copies for a project spawning before its
  first `pm upgrade`), and skills pm doesn't resolve at all — the harness
  does. No harness reads pm's agents dir, so after every install each tier's
  store is **projected** into the harness's own layout
  (`skills::install_global`/`project_assets` → `Harness::project_assets`, a
  copy into `~/.claude/{agents,skills}` and `main/.claude/{agents,skills}` for
  claude-code, `Harness::global_config_dir` naming the global one).
  Projection overwrites same-named files (canonical wins, a note is printed
  when a non-bundled file is replaced) and never deletes; `.claude/` itself is
  never removed — users hand-write defs there. Feature worktrees are seeded
  (`commands/seed.rs`) with the project tier only; global assets are read from
  home. A one-shot migration (`skills::migrate_project_to_global`, marker
  `.pm/migrations/global-assets`, run by `upgrade_project`) deletes the
  bundled copies earlier releases wrote into main, every feature worktree, and
  `.pm/workflows/`, since those would shadow the global tier; after the marker
  a bundled-named project file is a custom and is never touched again.
  Claude Code's skill precedence is personal > project — the one place
  project-shadows-global can't be delivered by placement — so
  `Harness::project_skill_shadowed_by_global` feeds a `pm doctor` finding
  instead.
- **Portability** — `path_utils.rs` swaps `~/` ↔ `$HOME` so registry state
  moves between machines.
- **Harness** (`harness/`) — the agent CLI pm launches, behind a `Harness`
  enum (`ClaudeCode`, `Codex`; string forms `claude-code`, `codex`). Each
  seam is a `match` in `harness/mod.rs`, never a trait: `build_cmd(&SpawnSpec,
  &HarnessConfig)` turns the harness-neutral spawn description (definition,
  prompt file, prompt, resume/fork, permission mode, model, writable dirs)
  plus the `[harness.<name>]` config into the command line — permission mode
  and model id are in the harness's own terms and pass through unvalidated,
  deliberately (no pm vocabulary to maintain per harness);
  `config_dir`/`seeded_files`/`projected_dirs`/`user_settings_file` describe
  the harness's own layout (`seeded_files` is the project's own settings and
  permissions, never hooks — and for Claude Code never `settings.local.json`,
  which Claude Code keeps only at the main checkout for every worktree, so
  `pm harness settings` skips it too);
  `project_assets` is the projection;
  `supports_prompt_delivery` is the composed-prompt capability probe;
  `trust_worktree`/`worktree_trusted`/`hook_trusted`/`malformed_hook_events`
  are the trust gates (inert for Claude Code); and `session_start_output`
  is the hook-side prompt channel. Harness-specific knowledge lives only in
  `harness/<name>.rs`; `agent_spawn` and the registry stay neutral. The
  user-facing surface is `pm harness
  hooks|skills|agents|settings|migrate|export|import|list|probe` — the
  CC-only `settings|migrate|export|import` take `--harness` (default
  `claude-code`; `claude_settings/migrate/export/import` keep their names
  because they *are* CC-specific and refuse `codex`) — with `pm claude …`
  as a hidden alias for one release. `hooks_install` writes into every
  in-use harness's `user_settings_file` (both take Claude Code's nested
  `hooks` shape); it recognises the previous `pm claude hooks …` spelling
  and the unguarded `pm harness hooks …` as pm-owned.
  `harness::harnesses_in_use` resolves `[agents.harness]` per key with the
  spawn's project-over-global, `""`-masks precedence, so a project masking a
  globally configured harness stops pm installing for it.

The sections below document the design decisions you can't recover by reading
the tree — these are the invariants to preserve.

### Agents as long-running message processors

pm agents are never-idle message processors, not one-shot scripts. This
is implemented with a **Stop hook** (`pm harness hooks stop`, installed by
`pm harness hooks install` into the user-level file of every harness in use
— `~/.claude/settings.json`, `$CODEX_HOME/hooks.json` — once per machine).
The installed command is `[ -n "$PM_AGENT_NAME" ] || exit 0; pm harness
hooks stop`: a user-level hook fires in every session of that harness on the
machine, and the guard keeps it inert in non-pm sessions without resolving
`pm` (`&&` would turn a false test into an exit-1 hook error). The hook
blocks until the agent's inbox has unread messages, then returns:

```json
{"decision": "block", "reason": "You have new messages. Run `pm msg read` …"}
```

The harness delivers this as a continuation prompt. The agent reads the
message, processes it, the turn ends, and the hook fires again — blocking
until the next message arrives.

Exception: if the Stop event reports a running background task or active
cron and no messages are queued, the hook returns `{}` (the documented
"allow" — on both harnesses any `decision` other than `block` fails Stop's
schema) instead of blocking so the running work isn't stalled. Recurring
crons stay active between fires, so an agent with one is message-delivered
only at fire boundaries. Codex's Stop payload carries neither field
(`additionalProperties: false`) and codex has no second wake source (a
completed background terminal does not wake the session), so `parse_busy`
is false there and codex agents block every turn.

Earlier releases wrote the hooks into `main/.claude/settings.json`, seeded
into each feature. `hooks_install` upserts the user-level file *first* and
then strips the project files: Claude Code merges both files and runs a
duplicated handler once, so no turn is ever hookless during the move. There
is no migration marker — a pm-owned entry in a project file is never the
user's, so recognise-and-remove is always correct and runs on every install
(`pm upgrade`, `pm doctor --fix`); `pm doctor` reports leftovers as
`StaleProjectHooks`.

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

#### Codex specifics

Codex (`harness/codex.rs`, minimum 0.153.2) has no launch-time role
channel, so the SessionStart hook (`pm harness hooks session-start`) does
double duty: it records the session id for every harness, and for a codex
agent also prints the composed prompt — definition body (front matter
stripped) + the same baseline/notice text `compose_spawn_prompt` writes — as
`hookSpecificOutput.additionalContext`. SessionStart fires with
`source: "resume"` too, so the role re-applies on `codex resume` without
`-c developer_instructions`. Codex reads `~/.agents/skills` and
`<repo>/.agents/skills` itself and never `.claude/`, so `projected_dirs` is
empty and `seed` copies the canonical store for every feature regardless of
harness; the "not projected" doctor finding is gated on
`Harness::projects_definitions`.

Two trust gates live in `$CODEX_HOME/config.toml`. Directory trust
(`[projects."<canonical path>"] trust_level = "trusted"`) pm writes at the
spawn chokepoint (`Harness::trust_worktree`, via `toml_edit` so the user's
file keeps its comments) and `pm doctor --fix` repairs. Hook trust
(`[hooks.state."<hooks.json>:<snake_case event>:<entry idx>:<hook idx>"]
trusted_hash`) only codex can write — the hash is not reproducible outside
the binary — via one interactive "Trust all and continue", re-asked when
the command text changes; without it codex runs no hook and says nothing,
so `pm doctor` asserts the entry (`IssueKind::HookUntrusted`) at pm's
actual entry indices (`hooks_install::pm_hook_position`; pm appends its
entries so a user's existing hooks keep their indices). A flat hooks.json
also parses and registers nothing (`IssueKind::HooksMalformed`).
`[harness.codex] bypass_hook_trust = true` is the explicit opt-out. Codex
accepts and honours the same `timeout` key, so pm's `86400` lets a Stop
hook block past the 600 s default.

The command is `codex -a <approval> -s <sandbox> [--add-dir …] [-m id]
[resume|fork <id>] [prompt]` — subcommands after global options. The
per-agent permission row is the `-s` sandbox mode, `[harness.codex]
sandbox` the harness-wide default, and `danger-full-access` the fallback:
the tmux socket is unreachable from inside any codex sandbox (Seatbelt
blocks `AF_UNIX` connect regardless of writable roots), so every
window-touching command fails there and a codex orchestrator needs full
access. `SpawnSpec.writable_dirs` (pm state dir, `main/.git`, pm config
dir, configured `writable_roots`) is emitted as `--add-dir` only when a
sandbox is on; under it reads, git, and message delivery work.
`-a never` is the default because an approval prompt in an unwatched tmux
window stalls the agent.

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

`pm feat new --context` without `--workflow` *defaults* to the bundled
single-agent `solo` workflow (`feat_common::DEFAULT_WORKFLOW`) — a context
needs a recipient. The default errors if `solo` isn't installed in either
tier; bare `feat new` (no context, no workflow) stays agentless.
`WorkflowDef::validate` enforces the contract: every team member must have a
definition file in a canonical store — `workflow::definition_paths` is
`main/.agents/agents/` then `~/.agents/agents/`; a harness's own dir is a
projection, never a source, so a def hand-written only in `.claude/agents/`
does not resolve — and `brief_agents ⊆` the team. A def present in a
canonical store but not yet projected passes validation while the harness
can't launch it, which `pm doctor` reports as a main-scope finding.
Exception: the reserved name `default` (`workflow::VANILLA_AGENT`, solo's
whole team) means a definition-less vanilla session — validation skips it,
and the spawn chokepoint passes no definition for it, unconditionally (a user
`default.md` is ignored). Earlier releases spelled the name `claude`; that
alias is removed. `pm upgrade` rewrites the bundled global `solo` to name
`default` and the migration deletes a stale project `.pm/workflows/solo`
naming `claude`, so an un-upgraded project repairs itself; `pm doctor` flags
an active registry entry still named `claude`
(`IssueKind::LegacyVanillaAgentName`) — it runs until its window dies but
restart/heal can't resolve it.

### Agent registry and the shared baseline

An agent registry entry's `active` flag is the single source of truth for its
lifecycle: `agent spawn` sets it true, `agent stop` false, and the
spawn/list/check/send paths read it. A separate `agent_definition` decouples the
registry key (display name / tmux window / `PM_AGENT_NAME`) from the
agent definition the harness launches, so several agents can run off one definition;
restart, fork, `pm open`, and the dead-window heal all preserve the alias.

The shared baseline is appended to every spawned agent's prompt through the
harness's prompt mechanism (`SpawnSpec.append_prompt_file` →
`--append-system-prompt-file` on Claude Code; SessionStart `additionalContext`
on codex), gated on the file existing at a single spawn chokepoint (older
projects without it spawn unchanged). Its content is general to all agents
and must **not** mention `.pm`. If a harness release drops that mechanism the
baseline would silently go dark, so `Harness::supports_prompt_delivery` probes
the installed binary (flag presence for Claude Code, minimum version for
codex): `pm doctor` warns for each harness in use when the baseline is
installed but unsupported, and `pm harness probe` reports the outcome
directly.

Per-agent `[agents.*]` settings are resolved at spawn time and deliberately not
stored on `AgentEntry` — re-reading config per spawn is what lets restart and
fork pick up edits. Precedence per setting: CLI flag (`--permission`,
`--model`) > project config > global config > unset (no flag passed). The
flags are spawn-only (`agent_spawn::SpawnOverrides`): `feat new`/`feat adopt`
apply them to the whole team, and restart/fork/heal don't carry them forward.

`[agents.harness]` is layered the same way (project > global per key, `""`
masks) and defaults to `claude-code`; any other value is an error at
resolution, never a silent fallback. It is the one setting that *is* stored on
`AgentEntry` (`harness`, serde-defaulted so older registries load unchanged):
a `session_id` only means something to the harness that produced it, so
`agent_spawn` resumes via `harness::resumable_session` only when the entry's
harness still matches config — otherwise it spawns fresh and says so — and
`agent fork` refuses outright (a fork without the transcript isn't a fork).
`[harness.<name>]` (`HarnessConfig`, `resolve_harness_config`) is the
per-harness counterpart with no per-agent shape, layered the same way and
resolved at the same chokepoint.

The **notice board** (`notice.rs`) is a seeded *directive* surface — terse
standing instructions hand-written into `notices.md` in the pm config dir and
`.pm/notices.md` (per-project). At the same spawn chokepoint,
`compose_spawn_prompt` folds any non-empty board onto the baseline into the
single `--append-system-prompt-file` (baseline → global → project). No command:
writing is manual file editing, reading is the seeding. When both boards are
empty it returns the baseline path unchanged, so the common case is unaffected.
Distinct from the info store (persistent knowledge, orchestrator-managed) and
`summary.md` (per-feature upward channel): the board pushes directives *down*
into agents, and agents reporting things go through `pm msg`, never the board.

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

Each feature leaves a `summary.md` in its worktree root for the orchestrator
who triages it after the branch is gone.
Each workflow's `workflow.md` names the single agent who owns
`summary.md`, stated in that role's section; content guidance is
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
- `AGENTS.md` — architecture, development guidelines, and any new conventions
  (`CLAUDE.md` is a symlink to it; edit `AGENTS.md`)

## Commits

- Commit messages: imperative, concise, focused on "why"
- One logical change per commit
