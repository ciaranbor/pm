# pm

Terminal-based project manager built around tmux and git worktrees.

pm gives every feature its own git branch, worktree, and tmux session, and a
team of agents (Claude Code, codex, or opencode) that talk to each other
through a file-based message queue. You tell a per-project orchestrator what
you want; it dispatches features, the agents implement, review, and report
back in their own sessions, and you merge.

Every command supports `--help` for its full flag reference. This README
covers how pm is used, the mental model, and the parts `--help` can't give
you.

## Requirements

- tmux, with a [Nerd Font](https://www.nerdfonts.com) (v3) as your terminal
  font for the plugin's badges
- git
- an agent harness: [Claude Code](https://claude.com/claude-code) (the
  default), codex, or opencode
- [gh](https://cli.github.com/), only for the PR commands (`pm feat pr`,
  `pm feat review`, `pm feat sync`)

## Install

```sh
cargo install --path .
```

Installs the `pm` binary to `~/.cargo/bin/` (ensure it's on your `PATH`).
Once pm's own source is a registered pm project, `pm self-update` pulls,
rebuilds, and upgrades every project.

Then add pm's tmux plugin: one line in your tmux config, after any `@pm-*`
options (see [tmux integration](#tmux-integration)) and any `bind s` or
`bind w` of yours, since init reads them as they stand when it runs:

```tmux
run-shell 'pm tmux init'
```

The plugin is part of the binary, so it always matches the installed pm.
tmux runs it with the server's environment, not your shell's, so `pm` must
be on the `PATH` the server started with; set `@pm-bin` to its full path
otherwise. Init only adds to your config and is safe to re-run on a reload.
It makes prefix `s` / `w` pm's tree, puts an agent badge in each window's
status entry, and keeps pm's state current on the server. The summary of
what needs you (each attention kind's glyph and count) goes where you put
it; init leaves `status-right` alone, as it is your theme's:

```tmux
set -g status-right '#{E:@pm_summary} %H:%M'
```

## Using pm

### Set up a project

```sh
pm init ~/projects/myapp                                         # new repo
pm init ~/projects/myapp --git https://github.com/org/myapp.git  # clone
pm register ~/code/myapp --name myapp                            # existing repo (--move to restructure in place)
```

Each gives a project root with the repo in `main/`, a `.pm/` state
directory, and a `myapp/main` tmux session. pm records the repo's default
branch (`origin/HEAD`, else the checked-out branch) as the project's main
branch. Bundled skills, agents, and workflows install once per machine
([Asset tiers](#asset-tiers)). pm projects your `main/.agents/` customs
into `main/.claude/`, which is generated: gitignore it.

Each also starts the orchestrator, the `main` agent, in that session
(`--no-main` skips it; `pm agent spawn main` starts it later). `main`
dispatches features and keeps the project's
[information store](#information-store-and-summaries). Run one per project;
several projects side by side is the normal case.

### Dispatch work

Tell `main` what you want in plain language. It names the feature, picks a
[workflow](#workflows-and-agents) (`implement-and-review`, a `-qa-` variant
for a change with user-facing behaviour to exercise, a `research-` variant
when the approach is uncertain), and runs `pm feat new` with your request as
the brief. That creates the branch, worktree, and tmux session
(`myapp/<feature>`), spawns the workflow's team, and briefs it. `main` then
steps back: the team works and reports in its own session, not through
`main`.

You can dispatch by hand the same way:

```sh
pm feat new login --workflow implement-and-review --context "Implement login per #42"
pm feat new login --workflow implement-qa-review --context - <<'EOF'
Implement the login page.
- validate the email field
- add an integration test
EOF
pm feat new login                                   # bare feature, no agents
pm feat new child --base parent                     # stack on another feature
```

With `--context` but no `--workflow`, pm uses the single-agent `solo`
workflow. `pm feat adopt <branch>` takes over an existing branch the same
way, and `pm feat review <pr>` checks out a GitHub PR for the `pr-review`
workflow.

### Follow what needs you

Agents record where a feature stands with `pm feat status`: `wip` while
working, `blocked` when waiting on you (with the question), `ready` when
done and waiting on your merge or delete.

pm also sees, without the agent saying so, when an agent's harness shows a
dialog (a permission prompt, a question, a plan to approve) or sits at its
prompt where no message will wake it (you interrupted it, an API error ended
its turn). pm surfaces all of it in tmux:

- the status line's summary (a glyph and count per kind), and an alert on
  every attached client when a feature becomes blocked or ready, or an
  agent — `main` included — starts asking (except on a client already
  showing that agent's pane). A feature alerts once per episode — not
  again when its agent's question outranks its `ready` for a while, nor
  when its session is opened on a status it already had;
- pm's tree (prefix `s` / `w`), tmux's own tree with each session's
  activity (working, or how long it has been quiet), attention glyph and
  reason, and each agent's badge. Enter on a session goes straight to the
  pane of the agent it is waiting on;
- each window's badge: the agent busy, asking, unarmed, waiting on
  background work, idle, dead or stopped, and an envelope for unread
  messages. A `main` session carries its main agent's badge.

Behind the status line and the tree is the **attention view**: pm ranks
every feature by what it needs from you — `blocked` on a question, an agent
`asking` in a dialog, `cleanup` after its PR merged, `ready` to merge, an
agent `dead` or `unarmed`, or `stalled` (every agent idle while the feature
is still `wip`) — most urgent first, with a row for `main` when one of its
agents is asking, dead or unarmed. To see it as text, run `pm feat status`
in the `main` session (`--all` for every project; `pm status` prints it
too), and work down from the top row: answer what is blocked or asking,
merge what is ready, restart what is dead, prompt what is unarmed, and ask
a stalled team why it stopped. In a feature, `pm feat status` shows just
that feature: its status, blocked reason, last activity, and summary head.
[Attention view](#attention-view) has each kind's rule and the `--json` form
for scripts.

### Work with the agents

Type straight into an agent's window to answer a question, redirect, or
add work. Typing into a blocked feature's agent sets the feature back to
`wip`. Agents never sit idle: each waits for its next message and acts on
it ([Agents as message processors](#agents-as-message-processors)), so
`pm msg send <agent> "…"` from any pane also reaches it. You can split an
agent's window to work beside it: pm watches and jumps to the pane it
started the agent in, whichever pane is active. Restarting the agent
replaces only that pane; stopping or deleting it closes only that pane,
and the window, no longer named for the agent, keeps yours.

Agents also message across projects: `pm msg send main --project tools
"…"` reaches the `tools` project's orchestrator, so an agent can ask about
another project or request something of it without you relaying it.

When an agent misbehaves, `pm agent restart <name>` respawns it on the same
conversation; `pm agent spawn <name>` adds one to the feature. A restart
refuses an agent that is busy, asking, or running background work; with
`--force` it interrupts it and leaves it a message to resume.

### Finish a feature

When a feature is `ready`, its summary owner has written a summary for
`main`, and `main` has reviewed it for gaps while the team can still
answer. Then, from any pane of the feature's session:

```sh
pm feat merge              # merge into its base, then remove worktree, session, branch
pm feat delete             # or discard it
```

The feature is the one your CWD is in, and tmux moves you to the base's
session before the feature's goes. From elsewhere, name it:
`pm feat merge login`.

Either tells `main`, which triages the summary into the project's
information store (`.pm/docs/`): follow-up todos, issues, ideas, and
durable findings. Ask `main` what's in the store when deciding what to do
next.

`pm feat pr create` / `pm feat pr ready` and `pm feat sync` cover features
that go through a GitHub PR instead.

### Around a reboot

pm keeps running in tmux; there is nothing to restart day to day. After a
reboot, `pm open` recreates a project's missing sessions, respawns every
active agent on its conversation, runs the `restore`
[lifecycle hook](#lifecycle-hooks), and warns about drift `pm doctor`
finds; `pm open --all` does so for every registered project, skipping any
whose root is gone, and attaches only if run from inside a project.
`pm close` (`--all` for every project) tears the sessions down by choice,
without touching state; `pm open` brings them back. `pm delete`, by
contrast, removes the project from pm, and with `--force` deletes its
worktrees too — `main` included, unpushed history with it.

## Concepts

### Features and worktrees

A **feature** is a branch + worktree + tmux session, tracked in `.pm/`. Omit
`--base` and the base is detected from your CWD, so `pm feat new child`
inside a feature worktree stacks on it; stacked features merge into their
parent, not main. If the parent is merged or deleted first, the child's base
is gone: `pm feat delete --force` still removes it, but `merge` and the
non-forced `delete` refuse and say how to rebase it onto a live branch.

`merge` warns about a feature not marked ready, and refuses a feature or
base worktree with uncommitted changes or a paused rebase. `pm feat info`
shows a feature's paused rebase; `pm status` and `pm doctor` show one in any
worktree, main's included.

### Workflows and agents

Two decoupled layers:

- **Agent definitions** (`~/.agents/agents/<name>.md`, or
  `main/.agents/agents/` for one project) describe an agent's *job*: what it
  does and how it evaluates work. They carry no routing, and no tool
  allowlist — each agent has its harness's full tool set.
- **Workflows** (`<pm config dir>/workflows/<name>/`, or
  `<project>/.pm/workflows/` for one project) define a feature's *topology*:
  the team, who receives the brief, who hands off to whom, who reports to
  the user, who writes the summary.

So one `implementer` plays different roles in different features. Every
agent runs `pm workflow show` at the start of each task to learn its
routing.

| Agent | Job |
|-------|-----|
| **main** | The project orchestrator: dispatches features, reviews summaries, keeps the information store |
| **implementer** | Implements each message, runs tests, addresses reviewer feedback |
| **reviewer** | Diffs the branch against base, evaluates quality and correctness, sends feedback |
| **researcher** | Read-only; explores the problem space and sends a refined brief to the implementer |
| **qa** | Runs the change as a user would, inside the project skill's safety boundary (isolated by default); reports bugs to the implementer and testing gaps for the summary |

| Workflow | Routing |
|----------|---------|
| **solo** | A single vanilla agent owns the feature end to end (the default with `--context` and no `--workflow`) |
| **implement-and-review** | Implementer ↔ reviewer loop |
| **research-implement-review** | Researcher → implementer → reviewer |
| **implement-qa-review** | Implementer → qa → reviewer; each gate loops with the implementer |
| **research-implement-qa-review** | Researcher → implementer → qa → reviewer |
| **research-only** | Researcher explores and reports to the user |
| **pr-review** | Reviewer reviews a checked-out PR and reports to the user (`pm feat review`) |

"Reports to the user" means **in the agent's own tmux session**, not by
messaging `main`. `main` is a dispatcher, not a relay: it re-engages only to
review a ready feature's summary and to triage it once the feature is merged
or deleted.

The definition name `default` is reserved: a definition-less vanilla
session, even if a `default.md` exists. `solo`'s team is exactly this name.

A workflow directory holds `config.toml` (`description`, optional
`when_to_use`, `agents` = the team spawned at `feat new`, `brief_agents` =
those who receive the brief) and `workflow.md` (routing prose with one
`## <agent>` section each, naming the summary owner). To write your own,
copy a bundled one under a new name (see [Asset tiers](#asset-tiers));
`pm workflow list` shows what is installed and where from.

`pm agent spawn <name> --agent <def>` separates the display name from the
definition, so several agents can run off one definition (`frontend-dev`
and `backend-dev`, both `--agent implementer`).

### Agents as message processors

`pm init` and `pm upgrade` install a **Stop hook** into the user-level hooks
file of every supported harness (`~/.claude/settings.json` for Claude Code,
`$CODEX_HOME/hooks.json` for codex; opencode gets a
[plugin](#opencode-agents) that does the same), once per machine. After
every turn it blocks until the agent has unread messages, then hands the
harness a continuation prompt to read them. The agent processes the message,
the turn ends, and the hook fires again. The brief at feature creation is
just the first message.

Exception: while a Claude Code background task or session cron is running
and nothing is queued, the hook lets the turn end so that work isn't
stalled; its completion wakes the agent, which reads `background` until
then. Codex and opencode agents block every turn. The wait has a one-year
timeout, so it never times out in practice.

An agent whose turn ends any other way never re-enters the hook, so a
message to it waits until something prompts it: an interrupt, a rejected
dialog, an API error, or the hook itself ended by its harness (Esc while it
waits), which then says why in the harness's transcript. pm shows such an
agent as `unarmed`, and [`pm msg send`](#messaging) re-arms it when it can.
No hook reports a Claude Code agent interrupted mid-turn or a dialog it
rejected; pm reads that from the tail of the session's transcript instead.
A SIGTERM from any process other than
the harness leaves the hook waiting.

A second hook, on UserPromptSubmit, sets a blocked feature back to `wip`
when you type into one of its agents; pm's own prompts don't count.

A third, the status hook (`pm harness hooks waiting`), runs on the events
that open and close a harness's dialogs and end its turns without Stop, and
keeps each agent's `asking`/`unarmed` state. opencode's plugin reports the
same through it.

The hooks apply to every session of that harness on the machine, so each is
guarded on `PM_AGENT_NAME`: a session pm didn't spawn exits it at once,
without needing `pm` on its `PATH`. Reinstall with `pm harness hooks
install`; `pm doctor --fix` restores a missing one.

### Messaging

Agents communicate through a file-based queue: one inbox per agent, scoped
to the feature, holding an ordered queue per sender with a cursor.

```sh
pm msg send reviewer "ready for review"
pm msg send reviewer <<'EOF'              # markdown body via quoted heredoc, passed verbatim
## Review findings
Details here.
EOF
pm msg send main --scope main "x"        # agent in another scope of this project
pm msg send main --project pm "x"         # agent in another project
pm msg read                               # next unread from the oldest sender, advances the cursor
pm msg read --from b --index -1           # re-read without moving the cursor
pm msg reply "short reply"                # reply to the last-read message, across scopes
pm msg wait                               # block until a message arrives
pm msg list                               # the inbox, with cursor markers
```

A bare `read` takes one sender: the one whose earliest unread message is
oldest, ending with `N more senders pending: b, c — pm msg read --from b`
when others wait. History stays on disk. `pm msg send` never spawns an
agent: it errors on an inactive recipient, and respawns one whose window
died. To an `unarmed` recipient it types the prompt the Stop hook would
have given, but only when that agent's input line is empty (in vim mode it
presses `i` first to leave NORMAL mode), so it never touches a draft or
answers a dialog;
elsewhere the message waits. opencode agents never need it: pm's plugin
waits again after every turn.

Identity resolves as `PM_AGENT_NAME` (set at spawn) > `$USER` > `"user"`, so
spawned agents need no `--as-agent`.

### Information store and summaries

Each project has an information store at `.pm/docs/`: todos, issues, ideas,
findings (the default categories, in `categories.toml`; add your own).
`main` manages it and keeps it lean: completed items are deleted (git
history is the record), with durable learnings moved into `findings.md`
first. The store holds knowledge; messaging is a queue. Don't use one for
the other.

A feature's **summary** is the hand-off its workflow's summary owner writes
for `main` (`pm workflow show` says what belongs in it), kept at
`.pm/summaries/<feature>.md` (`pm feat summary path`), never on the branch.
`pm feat status ready` requires it and asks `main` to review it. Merging or
deleting the feature always tells `main` which happened (a deleted
feature's changes never landed), and the summary stays until `main` has
triaged and deleted it; until then `pm feat new` and `pm feat adopt` refuse
that feature name.

### Asset tiers

Bundled assets — skills, agent definitions, workflows, and the shared
baseline — install **once per machine** and are refreshed by `pm upgrade` /
`pm self-update`:

| Tier | Skills / agents / baseline | Workflows |
|------|----------------------------|-----------|
| Global (pm's, plus your machine-wide customs) | `~/.agents/{skills,agents}`, `~/.agents/pm-baseline.md` | `<pm config dir>/workflows/` |
| Project (your customs only) | `main/.agents/{skills,agents}` | `<project>/.pm/workflows/` |

Everything resolves **project tier first, then global**, by name, so a
project file with a bundled name overrides it for that project. Bundled
names are pm's in the global tier — `pm upgrade` rewrites them there — so
keep global customs under names of your own. Only the `.agents/` copy of a
definition counts; pm projects it into each harness's own directory where
the harness needs one. To customise a bundled agent for one project:

```sh
cp ~/.agents/agents/reviewer.md <project>/main/.agents/agents/reviewer.md
pm upgrade                      # projects it for the harness
pm harness pull <feature>       # existing features don't get it otherwise
```

For a workflow, copy `<pm config dir>/workflows/<name>/` into
`<project>/.pm/workflows/<name>/`.

**Skills are the exception.** Claude Code ranks *personal* skills above
project ones, and pm projects every bundled skill into `~/.claude/skills/`,
so a project copy under a bundled skill's name never applies. Customise a
bundled skill globally (accepting that `pm upgrade` rewrites it) or copy it
to a name of your own; `pm doctor` flags a shadowed project skill.

Project-specific procedures — how to run, test, or review *here* — go in a
project skill, `main/.agents/skills/<name>/`. Only a skill's description is
in view when the agent decides whether to load it, so put the trigger and
any always-on rule there. pm's own `.agents/skills/pm-sandbox/` is an
example.

**Features get main's customs when created, and not again.** `pm feat
new`/`adopt`/`review` copy main's custom skills, agent definitions and
`.claude/settings.json` into the new worktree; `pm upgrade` never modifies a
feature worktree. `pm harness pull [feature]` (`--dry-run` to preview)
brings later changes in. Neither writes over a file the feature's branch
tracks, nor restores one it deleted. A feature's own `.agents/skills/` is
projected for its harnesses on seed or pull; agent definitions stay main's
until merged, because pm resolves them from main.

### Shared baseline and notice board

Rules common to every agent live in one bundled `~/.agents/pm-baseline.md`,
appended to every spawned agent's system prompt (`main` included).

Standing directives of your own go on the **notice board**, composed onto
the baseline at spawn. Two hand-edited markdown files, no command:

- `notices.md` in the pm config dir — every project
- `.pm/notices.md` — this project

Keep them terse: every line reaches every agent on every spawn. Both live in
the git-backed state repos, so they sync with `pm state push` (`--global`
for the global one). Example:

```markdown
Hit a pm bug or quirk? Message pm's main agent briefly —
`pm msg send main --project pm '<what broke>'`. Don't try to fix pm from here.
```

### Lifecycle hooks

Each project is bootstrapped with **lifecycle hooks** under `.pm/hooks/` for
project-specific steps: `post-create.sh` (after `pm feat new`/`adopt`/
`review` creates a feature), `post-merge.sh` (after `pm feat merge`, or
`feat delete` of a feature whose PR merged), and an opt-in `restore.sh`
(when `pm open` recreates a session). They run asynchronously in a `hook`
tmux window of the session they concern — the new feature's for
`post-create`, the base's for `post-merge`, each recreated session's for
`restore` — with that session's worktree as the working directory. Use them
to install deps, run migrations, copy gitignored secrets into a new
worktree; remove a script to disable it. pm only writes a hook script that
is missing, so your edits survive upgrades.

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

## Configuration

Settings live in `<project>/.pm/config.toml`, or `config.toml` in the pm
config dir (`~/.config/pm/` on Linux, `~/Library/Application Support/pm/` on
macOS) for every project. Agent settings are re-read at every spawn, so
restart, fork, `pm open`, and a dead-window heal pick up edits; there is no
spawn-time flag. For a one-off change, edit the row and restart the agent,
or switch inside the session (`/model`).

```toml
[agents.models]              # alias or full id, in the harness's own terms
"*" = "opus"
reviewer = "gpt-5"

[agents.harness]             # "claude-code" (the default), "codex" or "opencode"
reviewer = "codex"

[agents.permissions]         # the harness's own mode string; unset passes none
reviewer = "read-only"       # codex's -s sandbox mode, as reviewer runs on codex

[project]
max_features = 6             # refuse feat new / feat adopt beyond 6 unmerged features
```

**Rows.** Keys are the `--agent` definition, not the display name: an agent
spawned as `frontend-dev --agent implementer` takes `implementer`'s row.
`"*"` applies to every agent without a row of its own. Per setting, the
first of these wins: project named row, project `"*"`, global named row,
global `"*"`; `""` masks the rows below it.

**Models and permissions** are in the harness's terms: `--model` /
`--permission-mode` values for Claude Code, `-m` / the `-s` sandbox mode for
codex, `<provider>/<model>` / a permission rule list for opencode. Claude
Code and codex get them as written, so a typo surfaces in the agent's
window. Permissions are optional; with Claude Code's own auto mode most
setups need none.

A model or permission row is bound to the harness configured under the same
key, looking from the row's own file down: a `reviewer` row to the
`reviewer` harness row (else that file's `"*"`, else the global file's,
else `claude-code`), a `"*"` row to the `"*"` harness row. A row bound to
another harness than the one the agent spawns on is dropped, and the spawn
line says why. So when you move an agent to another harness in the project
config, a global model row for it no longer applies — set the model next to
the new harness row. With a global `[agents.models] reviewer = "opus"` and a
project `[agents.harness] reviewer = "codex"`, the reviewer gets codex's
default model and the spawn line reads:

```
Spawned agent 'reviewer' in myapp/login:1 (global [agents.models] row for 'reviewer' is bound to claude-code, not codex — not applied)
```

**Harness.** Any value besides the three is an error at spawn; pm never
falls back. A conversation is resumed only on the harness that produced it,
so after a harness change the next respawn starts fresh (and says so) and
`pm agent fork` refuses. `pm harness list` shows the harnesses; `pm agent
list` each agent's.

**`[project] max_features`** caps a project's in-flight features (any not
merged or stale); the project value beats the global one, and unset means
no cap.

### Mixed-harness teams

A workflow's team can span harnesses: each member takes the harness its
`[agents.harness]` row names. A common shape is a second model family as
reviewer and a local model for qa:

```toml
[agents.harness]             # implementer has no row: claude-code
reviewer = "codex"
qa = "opencode"

[agents.models]
qa = "local/qwen"
```

`pm feat new` and `pm feat adopt --workflow` first check that each member's
harness can spawn it, find its definition, and wake it for messages, and
refuse before creating anything, naming each failing member and what is
missing. Not checked, because only running the harness would tell: that
codex still trusts the hook's current command, that the harness is logged
in, and that a model id resolves. `pm agent spawn` skips the check; `pm
doctor` reports the same problems for existing agents.

What each harness needs is under Reference:
[Claude Code](#claude-code-agents), [codex](#codex-agents) (one
interactive hook-trust step per machine), and [opencode](#opencode-agents)
(a model row per agent).

## Moving to another machine

`.pm/` holds a project's state (features, agents, messages, config,
summaries, docs); the pm config dir holds the project registry, global
config, notices, and your global workflows. Both can be git-backed.

On the old machine, once, each against a new, empty repo (`init --remote`
resets local state to a remote that already has commits):

```sh
pm state init --global --remote <registry-url>   # the registry
pm state init --remote <state-url>               # in each project
pm state backfill                                # record repo and state URLs in the registry
```

Then, at each move, from a shell outside pm's tmux sessions:

```sh
pm close --all                  # stop agents so nothing is written after the push
pm state push --global          # and pm state push in each project
pm harness export --all -o pm-claude-code.tar.gz   # conversations live in the harness, not .pm/
```

On the new machine, with pm installed:

```sh
pm state init --global --remote <registry-url>
pm restore                      # clone repos, pull state, recreate worktrees and sessions
pm harness import pm-claude-code.tar.gz   # after restore, so the worktrees exist
```

`pm harness export|import` take `--harness` (default `claude-code`); run
them once per harness your agents use, with a file per harness. An export
holds the sessions of main and every feature worktree; an import skips a
feature with no worktree here.
`pm harness migrate --from <old path>` does the same for a project moved on
one machine.

The registry repo syncs your global custom workflows but never the bundled
ones: its `.gitignore` carries a block pm regenerates, so `pm upgrade`
rewriting them never dirties it. If an earlier release committed them, `pm
upgrade` untracks them and stages the deletion for `pm state push --global`.
A machine that pulls that commit loses the bundled dirs until it runs `pm
upgrade`.

## Reference

### tmux integration

Plugin options, set before `run-shell 'pm tmux init'`:

| Option | Default | Effect |
|---|---|---|
| `@pm-bin` | `pm` | the pm binary tmux runs |
| `@pm-auto-refresh` | on | keep pm's options current with a background `pm tmux refresh` loop; pm pushes its own changes at once, so the loop only catches what happens outside pm. The loop also re-sets pm's formats when it starts or pm is upgraded, so a new pm reaches a running server without a config reload |
| `@pm-refresh-interval` | `30` | seconds between refreshes |
| `@pm-window-status` | on | put each agent window's badge just before the window name in `window-status-format` and `window-status-current-format`, keeping your theme's style for the name |
| `@pm-bind-tree` | on | turn prefix `s` / `w` into pm's tree, sorted by name, when they run tmux's default `choose-tree` |
| `@pm-attention-key` | unset | a prefix key opening pm's tree with only the sessions needing attention |

Badges are Nerd Font glyphs: an agent window's shows its [agent
state](#attention-view), a feature session's the attention it needs, with
the reason after it in pm's tree. A kind that means what a state means
shares its glyph. Follow a badge with a space in your own formats: some
terminals (Ghostty) draw a glyph small when the next cell isn't blank.

| Glyph | Colour | Agent state | Attention |
|---|---|---|---|
| `nf-fa-hand` | red | | `blocked` |
| `nf-fa-question_circle` | red | `asking` | `asking` |
| `nf-md-broom` | grey | | `cleanup` |
| `nf-fa-check_circle` | green | | `ready` |
| `nf-md-skull` | red | `dead` | `dead` |
| `nf-fa-bell_slash` | magenta | `unarmed` | `unarmed` |
| `nf-fa-pause` | yellow | | `stalled` |
| `nf-fa-gear` | green | `busy` | |
| `nf-fa-spinner` | green | `background` | |
| `nf-fa-hourglass_half` | grey | `idle` | |
| `nf-fa-stop` | grey | `stopped`, `closed` | |
| `nf-fa-envelope` | yellow | after the state: unread messages | |

With `@pm-bind-tree off`, or to put the tree on another key:

```tmux
bind T choose-tree -Zs -O name -F '#{E:@pm_tree_format}' "run-shell \"pm tmux jump --client '#{client_name}' '%%'\""
```

`pm tmux refresh` publishes every project's attention view as user options
for your own status line or formats. An option whose value goes away is
unset, text is escaped for formats, and each name is set at one scope only:

| Scope | Option | Value |
|---|---|---|
| feature or main session | `@pm_project`, `@pm_feature` | names; a `main` session has no `@pm_feature` |
| | `@pm_progress` | `wip`, `blocked` or `ready`; unset on `main` |
| | `@pm_attention` | the attention kind; unset for `none` |
| | `@pm_reason` | the attention detail, or for `stalled` what the attention view shows; unset without one |
| | `@pm_badge` | the kind's glyph, styled; unset for `none`; on `main`, its main agent's badge |
| | `@pm_activity` | the busy glyph while the scope is working, else how long it has been quiet (`2h`, styled); unset under 10 minutes, and on `main` while its badge already shows its main agent at work |
| | `@pm_alert_pending`, `@pm_alerted` | pm's own bookkeeping: a ready alert waiting for its team to go quiet, and the kinds already alerted on |
| agent window | `@pm_agent` | the agent's name |
| | `@pm_agent_state` | an [agent state](#attention-view) |
| | `@pm_unread` | unread message count |
| | `@pm_agent_badge` | the badge, styled; it resets with `#[default]`, so placed anywhere but the start of a format, follow it with your theme's style |
| global | `@pm_summary` | each kind's glyph and how many scopes need it, styled and joined by ` · `; unset when nothing needs attention |
| | `@pm_count` | scopes (features and mains) needing attention |
| | `@pm_tree_format` | pm's `choose-tree` line format, set by `pm tmux init` |

### Attention view

One row per feature, most urgent first: what it needs, each agent's state
(`name:state`, `+N` for unread messages; `no session` when its session is
closed) and a detail. A feature gets the first of these that applies:

| Attention | When | Detail |
|---|---|---|
| `blocked` | status `blocked` | `<agent>: <question>`, the agent that set it (in JSON, `agent` and `detail`) |
| `asking` | an agent's harness shows a dialog: a question, a permission prompt, a plan to approve, or a startup prompt (folder or hook trust, login) still up a minute after spawn | `<agent>: <what it asks>` |
| `cleanup` | lifecycle `merged` or `stale`: delete it | `PR merged` or `stale` |
| `ready` | status `ready`, or PR `approved`; while an agent is busy it is left out of `@pm_summary` and `@pm_count`, and its alert waits until none is | the summary's first line, or `PR approved` |
| `dead` | an agent's window is gone from an open session, or its harness exited | `<agent>: window missing` or `<agent>: harness exited` |
| `unarmed` | an agent sits at its prompt where no message wakes it ([why](#agents-as-message-processors)) | `<agent>: <cause>` |
| `stalled` | status `wip`, agents running, all idle with no unread messages: the team stopped without saying why | `every agent idle, no unread messages` (`null` in JSON) |

Anything else shows its status. A `main` scope has no status: it gets a row
only when one of its agents is `asking`, `dead` or `unarmed`, in that
order. An agent is `idle` (waiting for a message), `busy` (mid-turn),
`asking`, `unarmed`, `background` (its turn ended for background work that
will wake it), `dead`, `stopped` (`pm agent stop`), or `closed` (its
feature's session is closed; `pm open` respawns it). A dialog you reject
can read `asking` until you next type, since Claude Code reports no
rejection. A scope is working while a busy or background agent showed
activity in the last 20 minutes; otherwise rows show how long it has been
quiet. PR state is what `pm feat sync` last recorded: the view never calls
GitHub, so it is cheap to poll.

`pm feat status --json` (with `--all`, or a feature name) prints the same
snapshot for tools to build on. `version` changes only when a field changes
meaning or goes away; new fields, attention kinds, agent states and waiting
kinds can appear within one, so a consumer must tolerate values it doesn't
know:

```json
{
  "version": 1,
  "projects": [{
    "name": "app",
    "root": "/src/app",
    "skipped": null,
    "main": {
      "session": "app/main",
      "session_exists": true,
      "agents": [],
      "attention": { "kind": "none", "detail": null, "agent": null },
      "working": false,
      "last_activity": null
    }
  }],
  "features": [{
    "project": "app",
    "name": "login",
    "attention": { "kind": "blocked", "detail": "which DB?", "agent": "implementer" },
    "progress": "blocked",
    "blocked_reason": "which DB?",
    "blocked_by": "implementer",
    "summary": null,
    "lifecycle": "wip",
    "pr": null,
    "session": "app/login",
    "session_exists": true,
    "agents": [{
      "name": "implementer",
      "state": "asking",
      "unread": 0,
      "window": "app/login:1",
      "waiting": { "kind": "question", "detail": "Postgres or SQLite?" }
    }],
    "working": false,
    "last_activity": "2026-10-02T09:30:00Z"
  }]
}
```

`features` is sorted like the rows. `attention.kind` is one of the table's
kinds or `none`; `skipped` says why a project's features are missing, and
`main` (its session and agents, shaped like a feature's) is then `null`,
and `root` empty if its registry entry is unreadable;
`summary` is the summary's first line whatever the status; `window` is the
agent's tmux target, `null` while it has none. `waiting` is what an
`asking`, `unarmed` or `background` agent is at (`kind` one of `question`,
`permission`, `plan`, `dialog`, `startup`, `interrupted`, `hook-ended`,
`error`, `prompt`, `tripped`, `background`), else `null`. `last_activity` is
the last time any of the scope's agents showed activity, `null` if none
ever has.

### Claude Code agents

The default harness; it needs no setup beyond `claude` on your `PATH`
(`pm harness probe` checks it).

- pm launches `claude --agent <def>` (no `--agent` for `default`), appends the
  [baseline](#shared-baseline-and-notice-board) with
  `--append-system-prompt-file`, and gives feature agents the summaries
  directory with `--add-dir`. It passes no `--permission-mode` unless an
  `[agents.permissions]` row sets one, so your own Claude Code default
  (auto mode, say) applies.
- A feature gets main's `.claude/settings.json` when created; `pm harness
  settings list|diff|pull|push|merge` compares and syncs the two later.
  `settings.local.json` is shared by every worktree at the main checkout,
  so pm leaves it alone.
- Personal skills outrank project ones, so a project copy of a bundled
  skill never applies ([Asset tiers](#asset-tiers)).

### Codex agents

Set `[agents.harness] <def> = "codex"` and pm spawns that agent in the codex
TUI. The loop, messaging, and skills are the same; what codex needs to run
unattended:

- **Hook trust — one interactive step per machine.** pm installs its hooks
  into `$CODEX_HOME/hooks.json` (creating `$CODEX_HOME` if needed), but
  codex runs no hook it has not been told to trust, and fails **silently**
  without it (the agent idles after its first turn). Start `codex` once in a
  trusted directory and choose **"Trust all and continue"** at the "Hooks
  need review" prompt. Codex asks again when a hook's command changes, so
  accept it again after an upgrade that changed one, or that added one (the
  status hooks did); until then the agent's startup reads `asking`. Without
  trust for the status hooks alone an agent still runs, but a dialog
  waiting on you reads as busy. The hooks file is
  global: the prompt appears in whichever codex session comes first,
  including your own non-pm ones. `pm doctor` reports a missing trust
  entry; the escape hatch is `[harness.codex] bypass_hook_trust = true`
  (`--dangerously-bypass-hook-trust`, one warning line per launch).
- **Directory trust** pm writes itself: each worktree gets a
  `trust_level = "trusted"` entry in `$CODEX_HOME/config.toml` at spawn
  (`pm doctor --fix` adds any missing).
- **No sandbox by default.** pm launches codex with `-a never -s
  danger-full-access`. pm's tmux socket cannot be reached from inside any
  codex sandbox, so a sandboxed agent cannot spawn, stop, restart, or heal
  other agents — a codex `main` needs full access. This is the blast radius
  pm's Claude Code agents already run with. To sandbox read-and-report
  agents anyway:

  ```toml
  [agents.permissions]          # for a codex agent, the -s sandbox mode
  reviewer = "workspace-write"

  [harness.codex]               # harness-wide, project beats global per key
  sandbox = "workspace-write"   # default for codex agents with no permissions row
  approval = "never"            # -a; "on-request" would stall an unwatched window
  writable_roots = ["main/target"]   # extra --add-dir; a project [] masks global
  ```

  pm always adds `--add-dir` for its state dir, the shared `main/.git`, and
  the pm config dir, so a sandboxed agent can read, run git, and message;
  every tmux-touching command fails.
- **Role delivery.** Codex has no `--agent`; the definition, baseline, and
  notice boards reach it through the SessionStart hook, on start and on
  every resume.
- **No shared daemon.** pm launches codex with `--no-daemon`, since hooks
  attached to codex's background server run in the server's environment,
  not the agent's. `pm doctor` reports a running agent that has recorded no
  session id after a grace period; `pm agent restart` it.
- `pm harness probe --harness codex` checks the version (0.156.0 or newer).

### opencode agents

Set `[agents.harness] <def> = "opencode"` and pm spawns that agent in the
opencode TUI (2.0.18 or later; `pm harness probe --harness opencode`
checks). What differs:

- **The loop is a plugin**, `pm-never-idle`, which pm installs under
  `~/.config/opencode/plugins/`. It stops itself after five turns in a row
  that read no message (a failing model, an unreadable inbox) rather than
  run away; `pm doctor` reports it with the last error. Fix the cause, then
  `pm agent restart <name>`.
- **A model row is required.** opencode silently swaps a model it can't
  resolve for its default, so pm refuses to spawn an opencode agent without
  an `[agents.models]` row and limits the agent to that row's provider and
  the providers pm config defines. A bad row fails the first turn with
  `Model unavailable` instead.
- **Providers are pm config**, as entries of opencode's own `providers`
  object. API keys are named by environment variable, never stored; a
  literal key is an error at spawn.

  ```toml
  [agents.harness]
  qa = "opencode"

  [agents.models]
  qa = "local/mlx-community/Qwen3.8-27B-4bit"

  [harness.opencode.providers.local]
  package = "@opencode/ai/providers/openai-compatible"
  settings = { baseURL = "http://127.0.0.1:8000/v1" }
  env = ["LOCAL_API_KEY"]       # optional: the variable holding the key
  ```

- **Permissions.** pm runs opencode with `--auto`, which approves whatever no
  rule denies; an `[agents.permissions]` row is opencode's rule list as a
  JSON array. `[harness.opencode] auto = false` makes it ask instead.
- **Always `--standalone`.** Without it, opencode commands share one server
  whose plugins act as whichever agent started it. If you run `opencode`
  yourself in an agent's window, pass `--standalone` too.
- An `enabled_providers` in your own `~/.config/opencode/opencode.json`
  overrides pm's provider restriction; `pm doctor` reports it.

## Development

`AGENTS.md` (Development) has the build, test, and lint commands. Tests
spawn real tmux sessions on a private server; the same section has the pty
budget and how to clean up leaked test servers.

Setting `PM_TMUX_SERVER=<name>` makes every `pm` command target that tmux
server (`tmux -L <name>`) instead of the default one; `pm open` run from a
pane of another server attaches a nested client rather than switching that
server's. `scripts/sandbox` uses it to give you a throwaway pm environment
for trying changes by hand (see `AGENTS.md`).

See `AGENTS.md` for architecture and development guidelines.
