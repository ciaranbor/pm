<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/branding/lockup-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/branding/lockup-light.svg">
    <img alt="pm" src="assets/branding/lockup-light.svg" width="196" height="84">
  </picture>
</h1>

Terminal-based project manager built around tmux and git worktrees.

pm gives every feature its own git branch, worktree, and tmux session, and a
team of agents (Claude Code, codex, or opencode) that talk to each other
through a file-based message queue. You tell a per-project orchestrator what
you want; it dispatches features, the agents implement, review, and report
back in their own sessions, and you merge.

Every command's `--help` has its flags; this README covers how pm is used
and the mental model.

## Requirements

- tmux 3.6 or later (older releases, such as Ubuntu 24.04's 3.4, break the
  plugin's status, alerts and attention key), with a
  [Nerd Font](https://www.nerdfonts.com) (v3) as your terminal font for the
  plugin's badges
- git
- an agent harness: [Claude Code](https://claude.com/claude-code) (the
  default), codex, or opencode
- [gh](https://cli.github.com/), only for the PR commands (`pm feat pr`,
  `pm feat review`, `pm feat sync`)

## Install

On macOS (Apple silicon) or Linux (x86_64):

```sh
curl -fsSL https://github.com/ciaranbor/pm/releases/latest/download/install.sh | sh
```

Run again, or run `pm self-update`, to upgrade in place; `pm --version`
says which pm you have. Elsewhere, or to run your own changes, build from
source (Rust 1.91 or later): `cargo install --path .` in a checkout, or
`cargo install --git https://github.com/ciaranbor/pm`. Such a build
reports its commit in its version (`0.2.0+3.gabc1234`), and `pm
self-update` leaves it alone unless given `--force`.

Then add pm's tmux plugin, one line at the end of your tmux config:

```tmux
run-shell 'pm tmux init'
```

It makes prefix `s` / `w` pm's tree, badges each agent's window, and puts
what needs you on the status line. [docs/tmux.md](docs/tmux.md) has its
options and what to do if tmux can't find `pm`.

### Android app

On an arm64 phone, open the
[latest release](https://github.com/ciaranbor/pm/releases/latest), download
`pm-<version>-android-arm64-v8a.apk`, and open it; Android asks once to allow
installs from the browser. Google Play Protect then says "App blocked to
protect your device": that is expected for this app, so tap "Install
anyway". Pair it by running `pm serve install --pair <device>` on the server
and scanning the QR code in the app (or pasting its `url`, `device` and
`token` lines). After that, the app tells you when a new release is out:
tapping its notification downloads the APK, which you install over the app.
[Remote access](#remote-access) covers the rest.

## Using pm

### Set up a project

```sh
pm init ~/projects/myapp                                         # new repo
pm init ~/projects/myapp --git https://github.com/org/myapp.git  # clone
pm init --git https://github.com/org/myapp.git                   # clone into ./myapp
pm register ~/code/myapp --name myapp                            # existing repo (--move to restructure in place)
```

Each gives a project root with the repo in `main/`, a `.pm/` state
directory, and a `myapp/main` tmux session. The project is named after its
directory (or `--name`), a name no other registered project may have. pm
records the repo's default branch (`origin/HEAD`, else the checked-out
branch) as the project's main branch. Bundled skills, agents, and workflows
install once per machine ([Customising](#customising)). pm projects your
`main/.agents/` customs into `main/.claude/`, which is generated: gitignore
it.

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
done and waiting on your merge or delete. pm also sees, without the agent
saying so, when an agent's harness shows a dialog or sits at its prompt
where no message will wake it. It surfaces all of it in tmux:

- the **status line**: a glyph, count and name per kind, and an
  announcement when a feature becomes blocked or ready, or an agent —
  `main` included — starts asking (except in that agent's own window);
- **pm's tree** (prefix `s` / `w`): each session's activity, attention and
  each agent's state, labelled. Enter on a session goes straight to the
  pane of the agent it is waiting on;
- each window's **badge**: the agent's state, and an envelope for unread
  messages.

Behind them is the **attention view**, which ranks every feature by the
first of these that applies, most urgent first:

| Kind | When |
|---|---|
| `blocked` | status `blocked`: an agent asked you a question |
| `asking` | an agent's harness shows a dialog: a question, a permission prompt, a plan to approve, or a startup prompt (folder or hook trust, login) still up a minute after spawn |
| `cleanup` | its PR merged, or it went stale: delete it |
| `ready` | status `ready`, or PR approved, and no agent busy: merge it |
| `dead` | an agent's window is gone from an open session, or its harness exited |
| `unarmed` | an agent sits at its prompt where no message wakes it ([why](#agents-are-message-processors)) |
| `stalled` | status `wip`, but every agent is idle with no unread messages: the team stopped without saying why |

`main` gets a row too when one of its agents is asking, dead or unarmed.
Run `pm feat status` in the `main` session (`--all` for every project on
this machine; `pm status` prints it too) and work down from the top:
answer what is blocked or asking, merge what is ready, restart what is
dead, prompt what is unarmed, and ask a stalled team why it stopped. In a
feature, `pm feat status` shows just that feature. Its `--json` form is a
stable contract for scripts ([docs/remote-api.md](docs/remote-api.md#attention-snapshot)).

### Work with the agents

Type straight into an agent's window to answer a question, redirect, or
add work. Typing into a blocked feature's agent sets the feature back to
`wip`. An *idle* agent — one waiting for its next message — sits at its
prompt, so what you type starts a turn at once, and the agent waits for
messages again after it. `pm msg send <agent> "…"` from any pane reaches an
agent either way.

You can split an agent's window to work beside it: pm watches and jumps to
the pane it started the agent in, whichever pane is active. Restarting the
agent replaces only that pane; stopping or deleting it closes only that
pane, and the window, no longer named for the agent, keeps yours.

Agents also message across projects: `pm msg send main --project tools
"…"` reaches the `tools` project's orchestrator, so an agent can ask about
another project or request something of it without you relaying it.

When an agent misbehaves, `pm agent restart <name>` respawns it on the same
conversation; `pm agent spawn <name>` adds one to the feature. `pm doctor`
reports an agent whose harness has exited.

A spawn or restart succeeds once the agent's harness has started its
session. One whose harness exits at launch fails with what its window
shows. One whose harness runs 20s without starting its session has not come
up: held on a login or trust screen, or before its startup, drawing nothing
(on macOS, often a login keychain that isn't answering). It fails saying
which, and is left running, since it may still come up.

`pm agent restart --all` restarts every active agent of the scope, and
`--all --global` every scope of every project. Idle agents restart and dead
ones are respawned. One mid-turn, asking, or waiting on background work is
skipped unless `--force`, which interrupts it and tells it to resume; an
agent whose session is closed (a closed feature, a project not opened) is
skipped, leaving the session closed. Run from an agent's own pane, that
agent restarts last. Each harness's agents restart after one of them has
come up: if it does not, the rest on that harness are skipped, still
running, rather than restarted into the same wait. Each agent gets a line
and the run ends with a count; a failed restart, or one that did not come
up, makes it exit non-zero.

A running agent keeps what it was launched with: its definition, the
baseline and notice boards, its config rows, and on codex and opencode pm's
hooks. So `pm upgrade` ends by restarting each running agent that would
launch differently now — one of those changed since it started, or a pm
release changed what agents launch with. Idle agents restart on the same
conversation. A busy agent, and the agent running the upgrade, restart at
their next idle, once the turn ends with nothing unread; a turn is never
cut short. Left running and listed with the command that restarts them: one
whose configured harness changed (a restart would start its conversation
over), and one that would not relaunch — no model row, or codex hooks not
yet trusted. A dead agent picks up the change at its next spawn. `pm
upgrade` covers every registered project, since the bundled assets and
hooks it installs are the machine's; a project it can't upgrade gets its
own line and the rest go ahead. `--dry-run` judges agents against the assets
installed now, so it cannot list those only the new assets make stale, and
says so. `pm agent restart --all --stale` runs the same sweep by hand, `pm
doctor` names each stale agent, and `restart_agents = false` under
`[upgrade]` in the global config turns the upgrade's restarts, deferred
ones included, off. On macOS the upgrade first asks the login keychain,
which some harnesses (Claude Code, codex) read as they start: if it does
not answer within 5s, agents on those harnesses are left running, each
listed with the command that restarts it once the keychain answers; `pm
doctor` reports it too.

### Project notes

`pm notes` opens the project's notes in `$VISUAL` or `$EDITOR`: one
Markdown file, `.pm/notes.md`, outside every worktree, which `pm state push`
syncs with the rest of `.pm/`. Run it from any scope, or name another
project (`pm notes tools`). The phone app edits the same file through
`pm serve`, from a project's Notes button; a phone save made while the file
changed underneath is refused and shows both texts to keep one or merge.
Notes over 256 KB are edited with `pm notes` only.
A save from the phone replaces the file, so vim warns that it changed if
you have it open; `:set autoread` reloads it instead when you have no
unsaved changes.

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
`pm feat merge login`. A worktree pm can't fully remove (a locked file, say)
doesn't stop the rest: the feature goes, and pm names what is left to delete
by hand.

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
finds; `pm open --all` does so for every registered project on this
machine.

A registered project is *on this machine* when its root holds its `main/`
checkout; the registry can sync from a machine that has projects this one
doesn't. A command naming a project that isn't here, or run inside its
root, refuses and says to `pm restore <name>`; an all-project command skips
it; `pm doctor` warns about a root that has state but no checkout; and
`pm delete <name>` unregisters it, removing its `.pm/` and then its root if
nothing else is left there.
`pm close` (`--all` for every project) tears the sessions down by choice,
without touching state; `pm open` brings them back. `pm delete`, by
contrast, removes the project from pm, and with `--force` deletes its
worktrees too — `main` included, unpushed history with it.

### Remote access

`pm serve` serves the attention view to pm's Android app, and types what you
send from it into agents' panes: from the phone you can see what needs you,
read an agent's conversation, reply, interrupt, answer its dialogs — one
the app can't show as a card (a startup prompt, a codex approval) from its
terminal, with keys and typed text — and edit a project's
[notes](#project-notes). On a Mac, the `pm serve install --pair <device>` that pairs the
[Android app](#android-app) also runs `pm serve` as a LaunchAgent and puts
it behind `tailscale serve`; `pm serve status` says whether it runs and
what reaches it.

It listens only on loopback and reaches the phone through
[Tailscale](https://tailscale.com): the phone must be on your tailnet, so
with another VPN on instead (ProtonVPN, say) the app can't connect and
shows the last snapshot it read. Elsewhere than macOS, run `pm serve`
under your service manager and `tailscale serve --bg 7764` yourself.

Notifications reach the phone off the tailnet through a push distributor:
Google's, built into the app, or for notifications without Google, the
[ntfy](https://ntfy.sh) app (F-Droid or Google Play), installed before
pairing. With neither, the app polls the server only as often as Android
lets it — hours apart once the phone dozes. A permission prompt can be
answered from its notification ([docs/android.md](docs/android.md#notifications)).

A paired device may do everything the app offers: read everything, type
into agents, merge and delete features, restart agents, and open, close
and delete projects, as `pm feat merge`, `pm feat delete`, `pm agent
restart`, `pm open`, `pm close` and `pm delete` do without `--force`. `pm
serve pair --name <device>` pairs another phone, or one again after a
reinstall; `pm serve devices` lists the paired devices and `pm serve revoke
<device>` withdraws one at once, cutting it off mid-stream. The app's
"Forget this server" unpairs the phone on the server too when it can reach
it; when it can't, the server lists the phone until you revoke it.

[docs/remote-api.md](docs/remote-api.md) has the server's API, push and
logging; [docs/android.md](docs/android.md) has the app's updates,
notification delivery, and how to build it.

## Concepts

### Features and worktrees

A **feature** is a branch + worktree + tmux session, tracked in `.pm/`. Omit
`--base` and the base is detected from your CWD, so `pm feat new child`
inside a feature worktree stacks on it; stacked features merge into their
parent, not main. If the parent is merged or deleted first, the child's base
is gone: `pm feat delete --force` still removes it, but `merge` and the
non-forced `delete` refuse and say how to rebase it onto a live branch.

### Workflows and agents

Two decoupled layers:

- **Agent definitions** describe an agent's *job*: what it does and how it
  evaluates work. They carry no routing, and no tool allowlist — each
  agent has its harness's full tool set.
- **Workflows** define a feature's *topology*: the team, who receives the
  brief, who hands off to whom, who reports to the user, who writes the
  summary.

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
or deleted. [Customising](#customising) covers writing your own.

### Agents are message processors

Agents never sit idle: after every turn, pm's waiter waits until the agent
has unread messages, then wakes it to read them, so the brief at feature
creation is just the first message. The turn has ended meanwhile, so the
agent's prompt is yours. An agent left with no waiter — interrupted or
failed in a turn a message started, its harness ending the waiter, or a
codex turn interrupted at all — is `unarmed`: no message wakes it until
something prompts it. [`pm msg send`](#messaging) re-arms it when its input
line is empty and takes text (not vim NORMAL mode) and no one is using its
pane (a tmux mode such as copy or tree mode, or shown on an attached
client); otherwise the message waits for a later send. A loop that wakes an
agent turn after turn without its inbox draining stops itself and says so;
`pm agent restart` starts it again. pm installs the hooks once per machine
([docs/harnesses.md](docs/harnesses.md#hooks)); `pm doctor --fix` restores a
missing one.

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
died, or whose harness exited to the shell in a window nobody is looking
at. To an `unarmed` recipient it types the prompt the waiter would have
given, but only when that agent's input line is empty; elsewhere the
message waits.

Identity resolves as `PM_AGENT_NAME` (set at spawn) > `$USER` > `"user"`, so
spawned agents need no `--as-agent`. An agent's scope resolves from the
worktree it was spawned in (`PM_AGENT_WORKTREE`), not where its shell has
`cd`'d, for `pm msg` and pm's hooks alike.

### Information store and summaries

Each project has an information store at `.pm/docs/`: todos, issues, ideas,
findings (the default categories, in `categories.toml`; add your own).
`main` manages it and keeps it lean: completed items are deleted (git
history is the record), with durable learnings moved into `findings.md`
first. The store holds knowledge; messaging is a queue.

A feature's **summary** is the hand-off its workflow's summary owner writes
for `main` (`pm workflow show` says what belongs in it), kept at
`.pm/summaries/<feature>.md` (`pm feat summary path`), never on the branch.
`pm feat status ready` requires it and asks `main` to review it. Merging or
deleting the feature always tells `main` which happened, and the summary
stays until `main` has triaged and deleted it; until then `pm feat new` and
`pm feat adopt` refuse that feature name.

## Customising

### Agents, workflows and skills

Bundled skills, agent definitions, workflows, and the shared baseline
install **once per machine** and are refreshed by `pm upgrade`, which `pm
self-update` and the install script run:

| Tier | Skills / agents / baseline | Workflows |
|------|----------------------------|-----------|
| Global (pm's, plus your machine-wide customs) | `~/.agents/{skills,agents}`, `~/.agents/pm-baseline.md` | `<pm config dir>/workflows/` |
| Project (your customs only) | `main/.agents/{skills,agents}` | `<project>/.pm/workflows/` |

Everything resolves **project tier first, then global**, by name, so a
project file with a bundled name overrides it for that project. Bundled
names are pm's in the global tier — `pm upgrade` rewrites them there — so
keep global customs under names of your own. To customise a bundled agent
for one project:

```sh
cp ~/.agents/agents/reviewer.md <project>/main/.agents/agents/reviewer.md
pm upgrade                      # projects it for the harness
pm harness pull <feature>       # existing features don't get it otherwise
```

For a workflow, copy `<pm config dir>/workflows/<name>/` into
`<project>/.pm/workflows/<name>/` and edit its `config.toml` (the team)
and `workflow.md` (the routing). `pm workflow list` shows what is
installed and where from. `pm agent spawn <name> --agent <def>` runs
several agents off one definition; the reserved definition `plain` is a
vanilla session with none.

**Skills are the exception.** Claude Code ranks *personal* skills above
project ones, and pm projects every bundled skill into `~/.claude/skills/`,
so a project copy under a bundled skill's name never applies. Customise a
bundled skill globally (accepting that `pm upgrade` rewrites it) or copy it
to a name of your own; `pm doctor` flags a shadowed project skill.

**Disabling bundled items.** To keep a bundled agent, workflow, skill or
the baseline off the machine, list it under `[bundled.disable]` in the global
`config.toml` (see [Configuration](#configuration)):

```toml
[bundled.disable]
agents    = ["qa"]
workflows = ["research-only"]
skills    = ["pm"]
baseline  = true             # unset or false keeps it
```

The next `pm upgrade` removes each from the global tier and its harness
projections, and later upgrades leave it out; remove the entry to get it
back. A project custom under the same name still applies, and a disabled
skill's name becomes free for a project skill. A command that needs a
disabled item with no custom in its place refuses with an error naming the
setting, and `pm doctor` reports what still references one: a workflow team,
the `solo` default, `pm feat review`'s `pr-review` and `reviewer`, the `main`
orchestrator. A workflow whose team needs a disabled agent stays in `pm
workflow list`, marked unavailable. Without the baseline, agents aren't told
to run `pm workflow show` or how to use `pm msg`.

Project-specific procedures — how to run, test, or review *here* — go in a
project skill, `main/.agents/skills/<name>/`. Only a skill's description is
in view when the agent decides whether to load it, so put the trigger and
any always-on rule there. pm's own `.agents/skills/pm-sandbox/` is an
example.

**Features get main's customs when created, and not again.** `pm feat
new`/`adopt`/`review` copy main's custom skills, agent definitions and
`.claude/settings.json` into the new worktree; `pm upgrade` never modifies a
feature worktree. `pm harness pull [feature]` brings later changes in,
without writing over a file the feature's branch tracks or restoring one
it deleted. Agent definitions stay main's until merged, because pm
resolves them from main.

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

A `post-create.sh` that gives each new feature the main worktree's secrets,
if it has any, and its own dependencies:

```sh
#!/bin/sh
set -eu
if [ -f "$PM_MAIN_WORKTREE/.env" ]; then cp "$PM_MAIN_WORKTREE/.env" "$PM_WORKTREE/"; fi
npm install
```

A `post-merge.sh` that installs the merged main and pushes it, skipping
merges into a stacked feature's base. This one is pm's own, so it also
refreshes the projects' pm assets:

```sh
#!/bin/sh
set -eu
[ -z "$PM_FEATURE" ] || exit 0
echo "merged $PM_MERGED_FEATURE; installing and pushing"
cargo install --path .
pm upgrade
git push
```

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
qa = "local/qwen"

[agents.harness]             # "claude-code" (the default), "codex" or "opencode"
reviewer = "codex"
qa = "opencode"

[agents.permissions]         # the harness's own mode string; unset passes none
reviewer = "read-only"       # codex's -s sandbox mode, as reviewer runs on codex

[project]
max_features = 6             # refuse feat new / feat adopt beyond 6 unmerged features
```

**Rows.** Keys are the `--agent` definition, not the display name: an agent
spawned as `frontend-dev --agent implementer` takes `implementer`'s row.
`"*"` applies to every agent without a row of its own; `plain` keys the
vanilla agent. Per setting, the
first of these wins: project named row, project `"*"`, global named row,
global `"*"`; `""` masks the rows below it.

**Models and permissions** are in the harness's terms: `--model` /
`--permission-mode` values for Claude Code, `-m` / the `-s` sandbox mode for
codex, `<provider>/<model>` / a permission rule list for opencode. Claude
Code and codex get them as written, so a typo surfaces in the agent's
window. Permissions are optional; with Claude Code's own auto mode most
setups need none. A model or permission row applies only to the harness
configured under the same key in its own file or below, so when you move
an agent to another harness in the project config, set its model next to
the new harness row; the spawn line names any row it drops:

```
Spawned agent 'reviewer' in myapp/login:1 (global [agents.models] row for 'reviewer' is bound to claude-code, not codex — not applied)
```

**Harness.** A team can mix harnesses, as above: a second model family as
reviewer, a local model for qa. Any value besides the three is an error at
spawn; pm never falls back. A conversation is resumed only on the harness
that produced it, so after a harness change the next respawn starts fresh
(and says so) and `pm agent fork` refuses; `pm harness list` shows the
harnesses, `pm agent list` each agent's. **codex needs one interactive
hook-trust step per machine** and fails silently without it;
[docs/harnesses.md](docs/harnesses.md) has that and what each harness needs.

**`[bundled.disable]`** is global only; see [Disabling bundled
items](#agents-workflows-and-skills).

**`[upgrade] restart_agents`**, in the global config only: `false` stops
`pm upgrade` restarting stale agents, now or at their next idle ([Work with the
agents](#work-with-the-agents)).

**`[project] max_features`** caps a project's in-flight features (any not
merged or stale); the project value beats the global one, and unset means
no cap.

## Further reading

- [docs/tmux.md](docs/tmux.md) — the tmux plugin's options, badges, and
  the options it publishes for your own formats
- [docs/remote-api.md](docs/remote-api.md) — `pm serve`'s API, push, the
  transcript contract, and the attention snapshot's JSON
- [docs/android.md](docs/android.md) — the app's updates, notifications,
  and building and signing it
- [docs/harnesses.md](docs/harnesses.md) — pm's hooks, and what Claude
  Code, codex and opencode each need
- [docs/migration.md](docs/migration.md) — moving projects to another
  machine
- [docs/releasing.md](docs/releasing.md) — cutting a release
- [AGENTS.md](AGENTS.md) — architecture, invariants, and development

## License

pm is released under the [MIT License](LICENSE).
