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

Each gives you a project root with the repo in `main/` and a `.pm/` state
directory. pm records the repo's default branch (`origin/HEAD`, else the
checked-out branch) as the project's main branch. Bundled skills, agent
definitions, workflows, and the baseline install once per machine (see
[Asset tiers](#asset-tiers)), not per project. Anything you add per project
under `main/.agents/` is projected into `main/.claude/`, which is generated
— gitignore it. Then `cd <root>/main` and create a feature:

```sh
pm feat new login                                          # bare feature, no agents
pm feat new login --context "Implement login per #42"      # solo developer agent
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
`--workflow <name>` picks the agent team to spawn and who to brief; with
`--context` but no `--workflow`, pm defaults to the single-agent `solo`
workflow. See `pm feat new --help` for stacking, naming, and editor options.

## Concepts

### Features and worktrees

A **feature** is a branch + worktree + tmux session, tracked in `.pm/`. Omit
`--base` and the base is detected from your CWD, so `pm feat new child` inside
a feature worktree stacks on it (stacked features merge into their parent, not
main). If the parent is merged or deleted first, the child's base is gone:
`pm feat delete --force` still removes it, but `merge` and the non-forced
`delete` refuse and tell you how to rebase it onto a live branch.

The lifecycle: `pm feat new` → work → optionally `pm feat pr create` /
`pm feat pr ready` / `pm feat review` → `pm feat merge` (cleans up by default).
While it runs, the agents record where it stands with `pm feat status
wip|blocked|ready` — `blocked` when waiting on you, `ready` when done and
waiting on your merge or delete — and `pm feat list` / `pm status` show it
(`merge` warns about a feature not marked ready).
Inspection and housekeeping subcommands (`list`, `info`, `switch`, `rename`,
`rebase`, `delete`, `sync`) round out `pm feat` — see `pm feat --help`.
`merge` refuses a feature or base worktree with uncommitted changes or a
paused rebase; `feat info`, `pm status` and `pm doctor` show a paused rebase.

`pm register`, `pm feat adopt --from` and `pm feat rename` carry agent
sessions to the new path for every harness in use, and print what they did.
Rename restarts the feature's running agents, resumed on their sessions,
and keeps the feature's status and summary; messages `main` has yet to read
from it are re-addressed to the new name.

### Lifecycle hooks

Each project is bootstrapped with **lifecycle hooks** under `.pm/hooks/`:
`post-create.sh` (after `pm feat new`/`adopt`/`review` creates a feature),
`post-merge.sh` (after `pm feat merge`, or `feat delete` of a feature whose PR
merged), and an opt-in `restore.sh` (when `pm open` recreates a session). They
run asynchronously in a dedicated `hook` tmux window of the session they
concern — the new feature's for `post-create`, the base's for `post-merge`,
each recreated session's for `restore` — with that session's worktree as the
working directory. Edit them to install deps, run migrations, copy gitignored
secrets into a new worktree, etc.; remove a script to disable it. pm only
writes a hook script that is missing, so editing one is safe and later pm
releases never overwrite it.

Every hook receives its context as `PM_*` environment variables, scoped to
the hook process (concurrent projects never see each other's values):

| Variable            | Value                                                                 |
|---------------------|-----------------------------------------------------------------------|
| `PM_PROJECT_ROOT`   | project root — the directory holding `.pm/` and the worktrees         |
| `PM_MAIN_WORKTREE`  | the main worktree (`$PM_PROJECT_ROOT/main`)                           |
| `PM_WORKTREE`       | the worktree the hook concerns (its working directory)                |
| `PM_SESSION`        | the tmux session the hook window is in                                |
| `PM_FEATURE`        | feature owning `PM_WORKTREE`/`PM_SESSION`; **empty** (set, `""`) in main scope |
| `PM_MERGED_FEATURE` | `post-merge` only: the feature that was merged into `PM_WORKTREE`     |

`PM_FEATURE` is always set so `set -u` scripts stay safe; test it with
`[ -n "$PM_FEATURE" ]`. For a `post-merge` of a stacked feature it names the
base feature, not the merged one.

### Workflows and agents

Two decoupled layers:

- **Agent definitions** (`~/.agents/agents/<name>.md`, or
  `main/.agents/agents/` for one project) describe an agent's *job* — what it
  does, how it evaluates work. They carry no routing. pm projects them into
  each harness's own dir on `init`/`upgrade` where the harness needs that
  (Claude Code and opencode do; codex reads `.agents/` itself); only the
  `.agents/` copy counts as a definition.
- **Workflows** (`<pm config dir>/workflows/<name>/`, or
  `<project>/.pm/workflows/` for one project) define the per-feature
  *topology* — who hands off to whom, who reports to the user.

This lets the same `implementer` play different routing roles in different
features without forking its definition. Every agent ships with the
`pm-workflow` skill and runs `pm workflow show` at the start of each task to
discover its routing. `pm workflow list` shows installed workflows and the
tier each came from.

Bundled agents:

| Agent | Job |
|-------|-----|
| **implementer** | Drains its inbox, implements each message, runs tests, addresses reviewer feedback |
| **reviewer** | Diffs the branch against base, evaluates quality/correctness, sends feedback |
| **researcher** | Read-only; explores the problem space and sends a refined brief to the implementer |
| **qa** | Runs the change as a user would, inside the project skill's safety boundary (isolated by default); reports bugs to the implementer and testing gaps for the summary |

The definition name `default` is **reserved**: it always means a
definition-less vanilla agent session (no definition passed to the
harness), even if a `default.md` definition file exists. The bundled `solo`
workflow's team is exactly this name. Earlier releases spelled it `claude`;
that alias is gone — `pm upgrade` rewrites `solo` to name `default`, and
`pm doctor` flags any still-running agent named `claude` (it cannot be
restarted; stop it and respawn).

Bundled workflows:

| Workflow | Routing |
|----------|---------|
| **solo** | Single developer owns the feature end-to-end (default when `--context` is given without `--workflow`) |
| **implement-and-review** | Implementer drains tasks; reviewer ↔ implementer loop |
| **research-implement-review** | Researcher → implementer → reviewer |
| **implement-qa-review** | Implementer → qa → reviewer; each gate loops with the implementer |
| **research-implement-qa-review** | Researcher → implementer → qa → reviewer |
| **research-only** | Researcher explores and reports findings to the user |
| **pr-review** | Reviewer reviews a checked-out PR and reports to the user (used by `pm feat review`) |

Each workflow directory holds a `config.toml` (`description`, optional
`when_to_use` hint, `agents` = the full team spawned at `feat new` time,
`brief_agents` = the subset that receives the `--context` brief) and a
`workflow.md` (free-form routing prose, with `## <agent>` sections; names the
summary owner). The bundled workflow names are pm-owned: like agents,
skills, and the baseline they are **overwritten** by `pm upgrade`. Directories
under `.pm/workflows/` with other names are yours and are never touched;
`pm workflow list` tags each entry `[bundled]` or `[user]`.

"Reports to the user" means **in the agent's own tmux session**, where you read
it live — not by messaging the `main` orchestrator. `main` is a dispatcher, not
a relay: it spins up features and steps back, re-engaging only to triage a
feature's summary. Intra-feature handoffs (reviewer ↔
implementer, researcher → implementer) are what use messaging.

Agent defs carry no `tools:` allowlist — each inherits the harness's full
tool set (including skills). Real guardrails belong in the permissions layer
(see below), not a per-agent tool list.

Manage agents with `pm agent spawn|list|stop|restart|delete|fork`. `spawn
<name> --agent <def>` decouples the display/messaging identity from the agent
definition, so you can run several agents off one definition (e.g.
`frontend-dev` and `backend-dev` both `--agent implementer`). `fork` starts a
new agent from a copy of another's history. `spawn`, `stop`, `restart` and
`delete` act on the current scope, or on another with `--scope <feature|main>`.
See `pm agent --help`.

### Configuration

Settings live in `<project>/.pm/config.toml`, or `config.toml` in the pm
config dir (`~/.config/pm/` on Linux, `~/Library/Application Support/pm/` on
macOS) to apply across projects. A `"*"` row applies to every agent without a
row of its own. Per setting, the first of these wins: project named row,
project `"*"`, global named row, global `"*"`; `""` masks the rows below it,
unset means no flag is passed.

```toml
[agents.permissions]         # harness's own mode string
reviewer = "plan"

[agents.models]              # alias or full id, in the harness's own terms
"*" = "gpt-5"
reviewer = "opus"

[agents.harness]             # agent CLI: "claude-code" (the default), "codex" or "opencode"
"*" = "codex"
reviewer = "claude-code"
```

Permission modes and model ids are in the terms of the agent's harness:
`--permission-mode` / `--model` values for Claude Code, the `-s` sandbox
mode / `-m` for codex, a permission rule list / `<provider>/<model>` for
opencode. On Claude Code and codex pm passes them through as written, so a
typo surfaces in the agent's tmux window. On opencode a missing row or one
of the wrong shape is an error at spawn, and a model opencode cannot resolve
fails the agent's first turn (see [opencode agents](#opencode-agents)).

A model or permission row is bound to the harness configured under the same
key, looking from the row's own file down: a `reviewer` row to the `reviewer`
harness row (else that file's `"*"`, else the global file's, else
`claude-code`), a `"*"` row to the `"*"` harness row. A row bound to a
harness other than the one the agent spawns on is dropped and the spawn line
says why. In the example above, `gpt-5` reaches every codex agent and `opus`
the claude-code reviewer. Change an agent's harness in the project config and
a global row for that agent no longer applies — set it again next to the new
harness row: with a global `[agents.models] reviewer = "opus"` and a project
`[agents.harness] reviewer = "codex"`, the reviewer gets codex's default
model and the spawn line reads:

```
Spawned agent 'reviewer' in myapp-login:1 (global [agents.models] row for 'reviewer' is bound to claude-code, not codex — not applied)
```

Any other `[agents.harness]` value is an error at spawn — pm never falls back
silently. `pm harness list` shows what pm can spawn and `pm agent list` each
agent's harness; a stored session is only resumed on the harness that
produced it — change an agent's harness and its next respawn starts a fresh
session (and `pm agent fork` refuses).

Config is the only way to set these: there is no spawn-time flag, so a
restart, fork, or heal launches with the same settings as the first spawn.
For a one-off change, edit the row and spawn, or switch inside the session
(`/model`, `/permissions` on Claude Code and codex).

Keys are the `--agent` definition, not the display name: an agent spawned as
`frontend-dev --agent implementer` takes `implementer`'s row. The vanilla
`default` agent is an ordinary key; `"*"` is the only row that reaches every
agent.

### Mixed-harness teams

A workflow's team can run on several harnesses at once: each member takes
the harness its `[agents.harness]` row names.

```toml
[agents.harness]             # implementer has no row: claude-code
reviewer = "codex"
qa = "opencode"

[agents.models]
qa = "local/qwen"
```

`pm feat new` and `pm feat adopt --workflow` first check that each member's
harness can spawn it, find its definition, and wake it for messages, and
refuse before creating anything, naming each failing member, its harness, and
what is missing. Not checked: that an installed hook's command is exactly the
one pm would write (an entry counts as pm's by the command it names), and,
because only running the harness would tell, that codex's trust entry still
matches the hook's current command text, that the harness is logged in, and
that a model id resolves.

`pm agent spawn` does not run this check; `pm doctor` reports the same
problems for a project's existing agents.

### Codex agents

Set `[agents.harness] <def> = "codex"` and pm spawns that agent in the
codex TUI instead of Claude Code. The same never-idle loop, messaging, and
skills apply; the differences are what codex needs before it will run
unattended:

- **Hook trust — one interactive step per machine.** `pm init`/`pm upgrade`
  install pm's hooks into `$CODEX_HOME/hooks.json`, but codex runs no hook it
  has not been told to trust, and it fails **silently** when trust is
  missing (the agent just idles after its first turn). Start `codex` once in
  a trusted directory and choose **"Trust all and continue"** at the "Hooks
  need review" prompt. Codex asks again only when a hook's command text
  changes. `pm doctor` reports a missing trust entry; the escape hatch is
  `[harness.codex] bypass_hook_trust = true` (passes
  `--dangerously-bypass-hook-trust`, one warning line per launch).
  The hooks file is global, so the prompt appears once in *any* codex
  session on the machine — including your own non-pm codex work, and even
  if no pm project uses codex, since pm installs its hooks for every
  harness it supports.
- **Directory trust** pm writes itself: each worktree gets a
  `[projects."<path>"] trust_level = "trusted"` entry in
  `$CODEX_HOME/config.toml` at spawn (`pm doctor --fix` adds any missing for
  a worktree whose agents or workflow team run on codex).
- **No sandbox by default.** pm launches codex with `-a never -s
  danger-full-access`. pm's tmux socket cannot be reached from inside any
  codex sandbox (macOS blocks the Unix-socket connect independently of
  writable roots), so a sandboxed agent cannot spawn, stop, restart, or heal
  other agents — a codex `main` needs full access. This is the same blast
  radius pm's Claude Code agents already run with; if you chose codex *for*
  its sandbox, know that pm turns it off unless you say otherwise:

  ```toml
  [agents.permissions]          # for a codex agent this is the -s sandbox mode
  implementer = "workspace-write"

  [harness.codex]               # harness-wide, project beats global per key
  sandbox = "workspace-write"   # default for codex agents with no permissions row
  approval = "never"            # -a; "on-request" would stall an unwatched window
  writable_roots = ["main/target"]   # extra --add-dir; a project [] masks global
  ```

  A sandboxed codex agent can read, run git, and send and receive messages
  — pm always adds `--add-dir` for its own state dir, the shared `main/.git`,
  and the pm config dir — but every tmux-touching command fails. Feature
  agents that only read and report fit that mode; orchestrators do not.
- **Role delivery.** Codex has no `--agent`; the agent's definition, the
  baseline, and the notice boards reach it through the SessionStart hook as
  developer context, on start and on every `codex resume`.
- **No shared daemon.** pm always launches codex with `--no-daemon`: attached
  to codex's background server, hooks run in the server's environment
  instead of the agent's, and the agent idles unreachable after its first
  turn. `pm doctor` reports a running agent that has recorded no session
  id after a grace period; restart it with `pm agent restart <name>`.
- `pm harness probe --harness codex` checks the installed version (0.156.0
  or newer, the first with `--no-daemon`). `pm harness settings` is Claude
  Code only.

### opencode agents

Set `[agents.harness] <def> = "opencode"` and pm spawns that agent in the
opencode TUI (2.0.18 or later). Messaging and skills work as on the other
harnesses; what differs:

- **The never-idle loop is a plugin.** opencode has no Stop hook, so `pm
  init`/`pm upgrade` install `pm-never-idle` under
  `~/.config/opencode/plugins/` (or `$XDG_CONFIG_HOME/opencode/plugins/`).
  When the agent's turn ends it waits for a message exactly as the Stop hook
  does, then prompts the session. It does nothing in a session pm didn't
  spawn. The files are pm's: an upgrade overwrites them, and opencode
  reloads the plugin in running agents when it does.
- **The loop stops itself rather than run away.** If five turns in a row are
  prompted for unread messages and read none — the model fails every turn, or
  the agent cannot read its inbox — the plugin stops prompting, says so in the
  session, and `pm doctor` reports the agent, with the error of the last turn
  when it failed (`Model unavailable: …`, the endpoint's own refusal). After a
  failed turn it waits 30 seconds before asking again. Fix the cause, then
  `pm agent restart <name>`.
- **Always `--standalone`.** pm launches `opencode --standalone --auto
  --session <id>`, with the session created beforehand. Without
  `--standalone` every opencode command shares one background server per
  `$HOME`, and that server's plugins act under the identity of whichever
  agent started it. If you run `opencode` yourself in an agent's window,
  pass `--standalone` too.
- **An unknown agent name is silent.** opencode reads definitions from
  `~/.config/opencode/agents/` and `<worktree>/.opencode/agents/`, where pm
  projects them, and runs its built-in prompt — no error — for a name it finds
  in neither. So an opencode spawn, and `pm feat new`'s check, refuse a
  definition with no projected copy, as `pm doctor` flags it for main and
  each feature worktree. `pm upgrade` projects it into main and the global
  dir; a feature worktree takes main's copy from `pm harness pull <feature>`,
  which `pm doctor --fix` runs.
- **Permissions.** `--auto` approves whatever no rule denies, because an
  approval prompt in an unwatched window stalls the agent. An
  `[agents.permissions]` row is opencode's own rule list as a JSON array;
  the last matching rule wins:

  ```toml
  [agents.harness]
  reviewer = "opencode"

  [agents.models]               # "<provider>/<model>"
  reviewer = "anthropic/claude-opus-5"

  [agents.permissions]
  reviewer = '[{"action":"edit","resource":"*","effect":"deny"}]'

  [harness.opencode]            # harness-wide, project beats global per key
  auto = false                  # drop --auto; asks then wait for an answer
  binary = "/opt/opencode/bin/opencode"   # default: opencode from PATH
  ```

  With `auto = false` pm adds
  `{"action":"external_directory","resource":"<dir>/*","effect":"allow"}`
  ahead of the row for its own state dir, the shared `main/.git`, and the pm
  config dir, so reading `.pm/` does not prompt — unless a later rule of the
  row matches those paths, which wins.
- **A model row is required, and binding.** Left to its config, opencode
  replaces a model it cannot resolve with its default one — hosted by
  opencode when no provider is configured — without an error. pm therefore
  refuses to spawn (or respawn, or fork) an opencode agent that has no
  `[agents.models]` row, pins the row on the agent's session, and limits the
  agent to the row's provider and the providers pm config defines. A row
  opencode cannot resolve then fails the agent's first turn (`Model
  unavailable`) before any request is made, and the loop stops and reports
  it as above.
- **Providers are pm config.** Each `[harness.opencode.providers.<id>]`
  table is an entry of opencode's own `providers` object and is written to
  every opencode agent's config as it stands, so a subagent can run on
  another provider than its parent's. A project entry replaces the global
  one of the same id whole; an empty project table removes it.

  ```toml
  [agents.harness]
  qa = "opencode"

  [agents.models]
  qa = "local/mlx-community/Qwen3.8-27B-4bit"

  [harness.opencode.providers.local]
  package = "@opencode/ai/providers/openai-compatible"
  settings = { baseURL = "http://127.0.0.1:8000/v1" }
  env = ["LOCAL_API_KEY"]       # optional: the variable holding the key
  # optional; quote ids holding `/` or `.`:
  models = { "mlx-community/Qwen3.8-27B-4bit" = { name = "Qwen" } }
  ```

  opencode resolves no model a provider does not list, so pm adds the row's
  model to its provider's `models`; a mistyped id therefore fails at the
  endpoint, not before. When the entry declares `models` and the row's id is
  not among them, the spawn line and `pm doctor` say so. They also flag an
  unquoted dotted id, which TOML splits into nested tables (`models.Qwen3.8-27B`
  is model `Qwen3` holding a table `8-27B`). A row may still name
  a provider pm config does not define — one opencode ships (`anthropic/…`)
  or one from your own `~/.config/opencode/opencode.json`: pm allows it and
  leaves its definition alone. Once pm config defines the same id, pm's
  entry replaces that file's whole.
- **Keys are named, never stored.** Name the variable — `env = ["NAME"]`,
  or `{env:NAME}` inside a value — and set it in the environment agents
  start in. An `apiKey` that is not exactly `{env:NAME}`, or an
  `Authorization` header without an `{env:…}`, is an error at spawn.
  opencode sends the request **without a key** when the variable is unset;
  the spawn line and `pm doctor` say when pm's own environment lacks it.
- **opencode's own config can undo the restriction.** opencode merges
  `~/.config/opencode/opencode.json[c]` and any `.opencode/` directory above
  the worktree into the agent's config. A provider restriction there
  (`enabled_providers`) replaces pm's: the agent's model becomes `Model
  unavailable` and that file's providers are allowed. `pm doctor` reports
  such a file, and an entry opencode dropped because a field has the wrong
  type (it drops unknown keys silently).
- **Forking needs a turn.** opencode refuses to fork a session that has had
  none, so `pm agent fork` fails for an agent that has not processed a
  message yet.
- pm writes the model, the providers and the permission rules to a config
  file of its own named by `OPENCODE_CONFIG`. An agent never inherits
  `OPENCODE_CONFIG` or `OPENCODE_CONFIG_CONTENT` from the shell that spawned
  it.
- **A session opencode no longer has** is not resumed: the respawn starts
  a fresh one and the spawn line says so, as after a harness change.
- **Every call pm makes to opencode is bounded** — a minute, five for a
  session export or import — and one that runs over is stopped and named,
  so a hung opencode fails the pm command instead of hanging it.
- **Moved worktree.** opencode cannot resume a session whose directory is
  gone. pm's own moves carry sessions (see Features and worktrees); after any other,
  stop the agent and run `pm harness migrate --harness opencode --from <old
  path>` in the new directory.
- `pm harness probe --harness opencode` checks the installed version. `pm
  harness settings` is Claude Code only.

### Agents as never-idle message processors

`pm init` and `pm upgrade` install a **Stop hook** into the user-level hooks
file of every supported harness (`~/.claude/settings.json` for Claude Code,
`$CODEX_HOME/hooks.json` for codex; opencode gets a
[plugin](#opencode-agents) that does the same), once per machine, so every
project on it is covered whichever harness it configures — `$CODEX_HOME` is created if
codex has never been run, and codex's one-time trust prompt then fires in
whichever codex session comes first. After every turn it blocks until the
agent has unread messages (calling `pm msg wait` internally), then returns a
`block` decision that the harness delivers as a continuation prompt. The
agent reads the message, processes it, the turn ends, and the hook fires
again. This turns every pm-managed agent into a never-idle processor:
`--context` at feature creation just queues the first message, delivered
exactly like any later peer message.

The hook applies to every session of that harness on the machine, so its
command is guarded on `PM_AGENT_NAME`: a session pm didn't spawn exits it
immediately, without needing `pm` on its `PATH`.

Exception: if a Claude Code background task or session cron is still running
and no messages are queued, the hook lets the turn end so the work isn't
stalled. Codex has no such second wake source, so its agents block every
turn; so do opencode's, where the plugin only starts waiting once the turn
is over.

Reinstall with `pm harness hooks install` (idempotent, works outside a
project); `pm doctor --fix` restores a missing one. Earlier releases wrote
the hooks per project; `pm upgrade` moves them out of the project files, and
`pm doctor` flags any left behind.

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
pm msg read                               # next unread from the oldest sender
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
- **One sender per read.** Without `--from`, the sender whose earliest unread
  message is oldest is picked; when others are also waiting the output ends
  with `N more senders pending: b, c — pm msg read --from b`. `--from`
  overrides the pick.

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

A feature's summary is the hand-off its workflow's summary owner writes for
the orchestrator (`pm workflow show` says what belongs in it), kept at
`.pm/summaries/<feature>.md` (`pm feat summary path`), never on the branch;
agents edit it like any other file (Claude Code agents are spawned with
`--add-dir` on that directory). `pm feat status ready` requires it and
messages `main` to triage it; cleanup hands `main` any summary it was not
told about. A worktree `summary.md` from an older feature is still collected
on cleanup; to mark such a feature ready, move it to that path first.

### Asset tiers

Bundled assets — skills, agent definitions, workflows, and the shared
baseline — install **once per machine** and are refreshed by `pm upgrade` /
`pm self-update`:

| Tier | Skills / agents / baseline | Workflows |
|------|----------------------------|-----------|
| Global (pm's, plus your machine-wide customs) | `~/.agents/{skills,agents}`, `~/.agents/pm-baseline.md` | `<pm config dir>/workflows/` |
| Project (your customs only) | `main/.agents/{skills,agents}` | `<project>/.pm/workflows/` |

Everything resolves **project tier first, then global**, by name: a project
file with a bundled name overrides it for that project. Bundled names are
pm's in the global tier — `pm upgrade` rewrites them there — so keep global
customs under names of your own. To customise, copy the bundled file and
edit:

```sh
cp ~/.agents/agents/reviewer.md <project>/main/.agents/agents/reviewer.md
pm upgrade                                     # projects it for the harness
pm harness pull <feature>                      # existing features don't get it otherwise
```

For workflows, copy `<pm config dir>/workflows/<name>/` into
`<project>/.pm/workflows/<name>/`. The pm config dir is `~/.config/pm/` on
Linux and `~/Library/Application Support/pm/` on macOS.

**Skills are the exception.** Claude Code ranks *personal* skills above
project ones, the inverse of its agent-definition precedence, and pm
projects every bundled skill into `~/.claude/skills/` — so a project copy
under a bundled skill's name never applies. Customise a bundled skill
globally (edit `~/.agents/skills/<name>/`, accepting that `pm upgrade`
rewrites it) or copy it to a name of your own. `pm doctor` flags a project
skill shadowed this way.

Project-specific procedures for agents — how to run, test, or review
*here* — go in a project skill of your own, `main/.agents/skills/<name>/`,
which `pm upgrade` projects for the harness. Only a skill's description is
in view when the agent decides whether to load it, so put the trigger and
any rule agents must always see there. pm's own
`.agents/skills/pm-sandbox/` is an example.

**Features get main's customs when they are created, and not again.**
`pm feat new`/`adopt`/`review` copy main's custom skills, agent definitions
and `.claude/settings.json` into the new worktree; `pm upgrade` never
modifies a feature worktree. A custom added to or changed in main afterwards
does not reach an existing feature until someone pulls it:

```sh
pm harness pull [feature]             # feature detected from CWD if omitted
pm harness pull [feature] --dry-run   # list what would be written
```

Both the creation-time copy and the pull take main's working copy, and
neither writes over a file the feature's branch tracks, so an edit committed
on a feature branch survives. A codex agent in that feature reads the edited
file directly. A Claude Code agent does not until the edit is merged and
projected from main: it loads the feature's `.claude/skills/`, which is
copied from main's projection.

Upgrading an existing project removes the per-project copies of bundled
assets that earlier releases installed — your own files are never touched.
The copies under `.pm/workflows/` are recoverable from `.pm/` git history
(commit the deletion with `pm state push`); the rest lived in generated,
gitignored directories.

### Shared agent baseline

Cross-cutting operating rules common to every agent — prose (brevity, no
mannered flourish), the comment/docs and test doctrine, the environment/CWD
conventions, the messaging heredoc form, the `pm workflow show` reminder,
surfacing out-of-scope problems, what "the user" means — live in a single
bundled `pm-baseline.md` rather than being repeated per agent.
`pm init`/`pm upgrade` install it to `~/.agents/pm-baseline.md`, and every
agent pm spawns (including `main`) has it appended to its system prompt
(`--append-system-prompt-file` on Claude Code; SessionStart hook context on
codex; the plugin's context hook on opencode).

### Notice board

Standing directives you want every spawned agent to obey, composed onto the
baseline at spawn time. Two hand-edited markdown files, no command — write or
remove notices by editing them directly:

- `notices.md` in the pm config dir — global, applies in every project
- `.pm/notices.md` — per-project

Keep them terse: every line is seeded into every agent on every spawn. Absent
or empty files seed nothing. Both live in the git-backed state repos, so they
sync via `pm state push` (`--global` for the global one). Example global
notice:

```markdown
Hit a pm bug or quirk? Message pm's main agent briefly —
`pm msg send main --project pm '<what broke>'`. Don't try to fix pm from here.
```

### State backup, sync, and restore

`.pm/` holds all project state (features, agents, messages, config, summaries,
docs) and the pm config dir holds project entries, cross-project config, and
the global workflow tier. Both can be git-backed:

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

The global registry repo syncs your global custom workflows but never the
bundled ones: its `.gitignore` carries a block pm regenerates from the
bundle, so `pm upgrade` rewriting them never dirties the repo. If an earlier
release committed them, `pm upgrade` untracks them and stages the deletion —
commit it with `pm state push --global`. A machine that pulls that commit
loses the bundled dirs from disk (and hits a conflict if its own copies
differ) until it runs `pm upgrade`, which reinstalls them.

See `pm state --help` and `pm restore --help`.

## Other commands

These round out the tool; each has its full flag reference under `--help`:

- `pm open` / `pm close` — recreate or tear down a project's tmux sessions
  without touching state (e.g. after a reboot). `pm open` also runs `pm
  doctor`'s checks and warns about unfixable drift.
- `pm status` / `pm doctor` — project dashboard; audit and auto-fix drift
  between pm state and git/tmux/GitHub reality.
- `pm harness` — the agent harness: `hooks`, bundled `skills`/`agents`
  (installed to `~/.agents/`, projected per harness), per-feature `settings`
  (`settings.json` only — Claude Code keeps `settings.local.json` at the main
  checkout for every worktree, so pm neither seeds nor syncs it), `pull`
  of main's customs into a feature (see Asset tiers), and
  `migrate|export|import` of session data across
  worktrees and machines (`--harness`, default `claude-code`); `list` the
  supported harnesses and `probe` the installed binary. `pm claude …`
  remains as a hidden alias for one release.
- `pm upgrade` / `pm self-update` — update bundled assets and the binary.
- `pm completions <shell>` — generate shell completion scripts.
- `pm list` — list registered projects.
- `pm delete` — full project teardown (sessions, `.pm/`, registry entry).
  Worktrees stay on disk as plain git checkouts; `--force` skips the safety
  checks and removes them, `main` included — that is the local repository, so
  history with no remote goes with it (warned about, never refused).
  Destructive — distinct from `pm close`, which only tears down sessions.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt
```

Tests spawn real tmux sessions on a private server; `AGENTS.md` (Development)
has the pty budget and how to clean up leaked test servers.

Setting `PM_TMUX_SERVER=<name>` makes every `pm` command target that tmux
server (`tmux -L <name>`) instead of the default one. `scripts/sandbox` uses
it to give you a throwaway pm environment for trying changes by hand (see
`AGENTS.md`).

See `AGENTS.md` for architecture and development guidelines.
