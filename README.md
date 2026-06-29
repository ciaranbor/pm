# pm

Terminal-based project manager built around tmux and git worktrees.

pm gives every feature its own git branch, worktree, and tmux session, and
optionally a team of Claude Code agents that talk to each other through a
file-based message queue. You dispatch work; the agents implement, review,
and report back in their own sessions.

Every command supports `--help` for its full flag reference — this README
covers the mental model and the parts you can't get from `--help`.

## Requirements

- tmux
- git
- [gh](https://cli.github.com/) — for the PR/review/sync commands (`pm feat pr`, `pm feat review`, `pm feat sync`)

## Install

```sh
cargo install --path .
```

Installs the `pm` binary to `~/.cargo/bin/` (ensure it's on your `PATH`).

## Quick start

Create a project — three ways, pick one:

```sh
pm init ~/projects/myapp                                   # new repo
pm init ~/projects/myapp --git https://github.com/org/myapp.git  # clone
pm register ~/code/myapp --name myapp                      # adopt an existing repo (--move to restructure in place)
```

Each gives you a project root with the repo in `main/`, a `.pm/` state
directory, and bundled hooks/skills/agents/baseline installed into
`main/.claude/`. Then `cd <root>/main` and create a feature:

```sh
pm feat new login                                          # bare feature, no agents
pm feat new login --workflow implement-and-review --context "Implement login per #42"
pm feat new child --base parent                            # stack on another feature

# Long brief via stdin (--context -), no approval prompt:
pm feat new login --workflow implement-and-review --context - <<'EOF'
Implement the login page.
- validate the email field
- add an integration test
EOF
```

`pm feat new` creates the branch, worktree, and tmux session (`myapp/login`).
`--context` requires `--workflow <name>` so pm knows which agent team to
spawn and who to brief. See `pm feat new --help` for stacking, naming, and
editor options.

## Concepts

### Features and worktrees

A **feature** is a branch + worktree + tmux session, tracked in `.pm/`. Omit
`--base` and the base is detected from your CWD, so `pm feat new child` inside
a feature worktree stacks on it (stacked features merge into their parent, not
main).

The lifecycle: `pm feat new` → work → optionally `pm feat pr create` /
`pm feat ready` / `pm feat review` → `pm feat merge` (cleans up by default).
Inspection and housekeeping subcommands (`list`, `info`, `switch`, `rename`,
`delete`, `sync`) round out `pm feat` — see `pm feat --help`.

Each project is bootstrapped with **lifecycle hooks** under `.pm/hooks/`:
`post-create.sh` (after `pm feat new`), `post-merge.sh` (after `pm feat merge`),
and an opt-in `restore.sh` (when `pm open` recreates a session). They run
asynchronously in a dedicated `hook` tmux window. Edit them to install deps,
run migrations, reopen an editor, etc.; remove a script to disable it.

### Workflows and agents

Two decoupled layers:

- **Agent definitions** (`main/.claude/agents/<name>.md`) describe an agent's
  *job* — what it does, how it evaluates work. They carry no routing.
- **Workflows** (`<project>/.pm/workflows/<name>/`) define the per-feature
  *topology* — who hands off to whom, who reports to the user.

This lets the same `implementer` play different routing roles in different
features without forking its definition. Every agent ships with the
`pm-workflow` skill and runs `pm workflow show` at the start of each task to
discover its routing. `pm workflow list` shows installed workflows.

Bundled agents:

| Agent | Job |
|-------|-----|
| **implementer** | Drains its inbox, implements each message, runs tests, addresses reviewer feedback |
| **reviewer** | Diffs the branch against base, evaluates quality/correctness, sends feedback |
| **researcher** | Read-only; explores the problem space and sends a refined brief to the implementer |

Bundled workflows:

| Workflow | Routing |
|----------|---------|
| **implement-and-review** | Implementer drains tasks; reviewer ↔ implementer loop |
| **research-implement-review** | Researcher → implementer → reviewer |
| **research-only** | Researcher explores and reports findings to the user |
| **pr-review** | Reviewer reviews a checked-out PR and reports to the user (used by `pm feat review`) |

Each workflow directory holds a `config.toml` (`description`, optional
`when_to_use` hint, `agents` = the full team spawned at `feat new` time,
`brief_agents` = the subset that receives the `--context` brief) and a
`workflow.md` (free-form routing prose, with `## <agent>` sections; names the
`summary.md` owner). Workflows use a **preserve** install policy — `pm upgrade`
adds missing ones but never overwrites your edits. Agents, skills, and the
baseline are **overwritten** (the bundle is authoritative).

"Reports to the user" means **in the agent's own tmux session**, where you read
it live — not by messaging the `main` orchestrator. `main` is a dispatcher, not
a relay: it spins up features and steps back, re-engaging only to triage a
feature's `summary.md` on cleanup. Intra-feature handoffs (reviewer ↔
implementer, researcher → implementer) are what use messaging.

Agent defs carry no `tools:` allowlist — spawned via `claude --agent`, each
inherits all Claude Code tools (including `Skill`). Real guardrails belong in
the permissions layer (`[agents.permissions]` in `.pm/config.toml`), not a
per-agent tool list.

Manage agents with `pm agent spawn|list|stop|restart|delete|fork`. `spawn
<name> --agent <def>` decouples the display/messaging identity from the claude
definition, so you can run several agents off one definition (e.g.
`frontend-dev` and `backend-dev` both `--agent implementer`). `fork` starts a
new agent from a copy of another's history. See `pm agent --help`.

### Agents as never-idle message processors

`pm init` installs a Claude Code **Stop hook** into
`main/.claude/settings.json`. After every turn it blocks until the agent has
unread messages (calling `pm msg wait` internally), then returns a `block`
decision that Claude Code delivers as a continuation prompt. The agent reads
the message, processes it, the turn ends, and the hook fires again. This turns
every pm-managed agent into a never-idle processor: `--context` at feature
creation just queues the first message, delivered exactly like any later peer
message.

Exception: if a background task or session cron is still running and no
messages are queued, the hook lets the turn end so the work isn't stalled.

Reinstall with `pm claude hooks install` (idempotent, append-only); `pm doctor
--fix` restores a missing one.

### Messaging

Agents communicate through a file-based queue, one inbox per agent scoped to
the feature. Each inbox holds an ordered queue per sender with a cursor
tracking the last message processed; `pm msg read` returns the next unread and
advances the cursor.

```sh
pm msg send reviewer "ready for review"
pm msg send reviewer <<'EOF'              # multi-line / markdown body via heredoc
## Review findings
Details here.
EOF
pm msg send impl@main "note"              # cross-scope: agent in another scope
pm msg read                               # next unread (auto-picks sender if unambiguous)
pm msg reply "short reply"                # reply to the last-read cross-scope message
pm msg wait                               # block until a new message arrives
pm msg list                               # enumerate inbox with cursor markers
```

Conventions worth knowing (the rest is in `pm msg --help`):

- **Use a quoted-delimiter heredoc** (`<<'EOF' … EOF`) for any body with
  markdown, backticks, `$`, or apostrophes — it's passed verbatim. Reserve the
  positional `"…"` form for trivial one-liners.
- **`read` reads *and* advances.** `--index <n>` (requires `--from`) re-reads a
  past message without moving the cursor; history stays on disk forever.
- **`--from` is needed only when ambiguous** — if only one sender has unread,
  it's auto-selected.

Identity resolves as `PM_AGENT_NAME` (set by `pm agent spawn`) > `$USER` >
`"user"`, so spawned agents need no `--as-agent`.

### Information store

Each project has an information store at `.pm/docs/` for project-level
persistent knowledge — todos, issues, ideas, findings (the default categories,
defined in `categories.toml`; add your own). The `main` orchestrator manages it
directly, keeping it lean: completed items are **deleted** (git history is the
record), with durable learnings migrated into `findings.md` first.

This is distinct from messaging: the store is a database for durable knowledge,
the queue is for cross-agent/cross-scope communication. Don't conflate them.

On `pm feat delete`/`merge`, a feature's `summary.md` is collected to
`.pm/summaries/<feature>.md` so the orchestrator can triage it into the store.

### Shared agent baseline

Cross-cutting operating rules common to every agent — brevity, the
environment/CWD conventions, the messaging heredoc form, the `pm workflow show`
reminder, what "the user" means — live in a single bundled `pm-baseline.md`
rather than being repeated per agent. `pm init`/`pm upgrade` install it to
`main/.claude/pm-baseline.md`, and every agent pm spawns (including `main`) is
launched with `claude --append-system-prompt-file <that path>`.

### State backup, sync, and restore

`.pm/` holds all project state (features, agents, messages, config, summaries,
docs) and the global registry at `~/.config/pm/` holds project entries and
cross-project config. Both can be git-backed:

```sh
pm state init --remote <url>     # init .pm/ repo, set remote, pull
pm state push                    # auto-commit and push
pm state init --global --remote <url>   # same for the global registry
pm state backfill                # record repo_url / state_remote for existing projects
```

This enables full machine migration — back both up to git, then on a fresh
machine:

```sh
pm state init --global --remote <global-registry-url>
pm restore                       # clone repos, pull state, recreate worktrees + sessions
```

See `pm state --help` and `pm restore --help`.

## Other commands

These round out the tool; each has its full flag reference under `--help`:

- `pm open` / `pm close` — recreate or tear down a project's tmux sessions
  without touching state (e.g. after a reboot). `pm open` also runs `pm
  doctor`'s checks and warns about unfixable drift.
- `pm status` / `pm doctor` — project dashboard; audit and auto-fix drift
  between pm state and git/tmux/GitHub reality.
- `pm claude` — manage and transfer Claude Code settings, session data, and
  bundled assets across worktrees and machines.
- `pm upgrade` / `pm self-update` — update bundled assets and the binary.
- `pm completions <shell>` — generate shell completion scripts.
- `pm list` — list registered projects.
- `pm delete` — full project teardown (sessions, `.pm/`, registry entry).
  Destructive — distinct from `pm close`, which only tears down sessions.

## Development

```sh
cargo build
cargo test
cargo clippy
cargo fmt
```

Tests spawn real tmux sessions. `cargo test` runs are capped at 4 threads via
`.cargo/config.toml` to keep pty usage well under macOS limits. To clean up
stale test servers: `for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L
$(basename "$s") kill-server 2>/dev/null; rm -f "$s"; done`.

See `CLAUDE.md` for architecture and development guidelines.
