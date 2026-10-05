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
  when its session is opened or closed on a status it already had. One
  whose session is closed still alerts when it becomes blocked or ready (a
  PR approved through `pm feat sync`, say);
- pm's tree (prefix `s` / `w`), tmux's own tree with each session's
  activity (working, waiting on background work, or how long it has been
  quiet), attention and reason, and each agent's state, every glyph
  labelled. Enter on a session goes straight to the pane of the agent it
  is waiting on;
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
`--force` it interrupts it and leaves it a message to resume. Every command
that launches a harness — these, `feat new`/`adopt`, `open`, `agent fork`,
and a `msg send` that respawns a dead window — waits a moment for it to stay
up, and reports one that exits at launch (a flag its CLI rejects, say) with
what its window shows; `pm doctor` reports an agent whose harness has since
exited.

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

### Remote access

`pm serve` serves the attention view to pm's phone app, and types what
you send from it into agents' panes. It
listens on `127.0.0.1` only (port 7764 by default, `[serve] port`
otherwise); `tailscale serve` puts it on your tailnet. One command sets it
up on a Mac:

```sh
pm serve install --pair pixel   # LaunchAgent, `tailscale serve`, then a QR code to scan in the app
pm serve status                 # whether it runs, and what reaches it
```

The LaunchAgent runs it at login, restarts it, and it follows upgrades:
`pm upgrade` rewrites an outdated plist, keeping what install chose.
Install runs `tailscale serve --bg <port>` itself (`--no-tailscale` not
to) when the tailnet has MagicDNS and HTTPS certificates on and nothing
serves its port 443 here; otherwise it changes nothing and says what to
do, as `pm doctor` does while a device is paired. Running it again repairs
an install; `pm serve uninstall` removes it and leaves `tailscale serve`
as it is. Elsewhere than macOS, run `pm serve` under your service manager
and `tailscale serve --bg 7764` yourself.

Nothing on the phone is urgent, so the server reads pm's state once a
minute, and every few seconds only while the app is open. A change pm
makes itself reaches it within a few seconds.

The phone must be on the tailnet to reach it: with Tailscale off (another
VPN, such as ProtonVPN, on instead) the app can't connect, and shows the
last snapshot it read. Every request
needs a paired device's bearer token, local ones included — through
`tailscale serve` every request arrives on loopback. `pair` prints the
token once, beside the QR code; `pm serve devices` lists the paired
devices and `pm serve revoke <device>` withdraws one's token, and its push
subscription, at once. A paired device reads everything and can type
into agents. pm
does not rely on Tailscale's identity headers: a tagged device sends none.
`pm serve` logs each request with its device to stderr — for input, the
keys pressed or the SHA-256 of the text, never the text — which the
LaunchAgent sends to `serve.log` (`pm serve logs`) in the `serve/` dir of
pm's config dir, beside the devices file and the server's VAPID key
(`vapid.pem`); `pm state` syncs none of them.

Notifications don't need the tailnet. A device subscribes through
[UnifiedPush](https://unifiedpush.org) — the ntfy app using ntfy.sh, or
Google's push service built into pm's app — and registers the
subscription with `pm serve`, which sends each `transition` event to it
as an encrypted Web Push (RFC 8030/8291, signed with the VAPID key). A
push carries only `{project, scope, kind, agent}`; the app fetches the rest
over the tailnet when opened. A push service answering that a
subscription is gone drops it. Deleting `vapid.pem` strands every
subscription until the app is next opened and subscribes again.

A subscription must be https on a known push service — Google's
(`fcm.googleapis.com`) or `ntfy.sh` — so a token can't aim `pm serve` at
a service on the tailnet or the Mac. A self-hosted distributor's host goes
in the global config, read as `pm serve` starts:

```toml
[serve]
push_hosts = ["ntfy.example.org"]
```

Pushes go only to public addresses, whatever a host resolves to, and
follow no redirect.

The API is under `/v1`; every path needs a paired device's token:

| Path | Returns |
|---|---|
| `snapshot` | `pm feat status --all --json` ([Attention view](#attention-view)) |
| `events` | server-sent events: `snapshot` (the snapshot, at connect and on each change), `transition` (`{project, scope, kind, detail, agent}` as a feature or `main` becomes blocked, asking or ready — alerted as tmux alerts — or an agent dies); with `?watch={project}/{scope}/{agent}[&after={cursor}]`, also `transcript` (below); a comment line every 25 s of silence |
| `features/{project}/{feature}/summary` | the feature's summary, Markdown |
| `agents/{project}/{scope}/{agent}/screen` | what the agent's pane shows now, plain text, row for row |
| `agents/{project}/{scope}/{agent}/transcript?before={cursor}&limit={n}` | the agent's conversation, a page back from `before` (the end when absent); `limit` 1–200, default 50 |
| `agents/{project}/{scope}/{agent}/transcript/result?ref={full}` | a tool result's whole output, plain text |
| `push` | `GET`: `{"vapid": <public key>}`, to subscribe against; `PUT` a Web Push subscription (`{"endpoint": <https URL>, "keys": {"p256dh", "auth"}}`) to push to this device; `DELETE` to stop |
| `agents/{project}/{scope}/{agent}/input` | `POST {"text"}` (up to 128 KB): typed into the agent's input line and submitted; `{"delivery": "sent", "confirmed"}` once submitted (`confirmed`: seen in the conversation within 5 s), or `{"delivery": "queued"}` when the agent is mid-turn and takes it as a step ends |
| `agents/{project}/{scope}/{agent}/interrupt` | `POST`: presses Escape, ending the agent's turn; refused while it waits for a message |
| `agents/{project}/{scope}/{agent}/keys` | `POST {"keys": [...]}`: presses each of `Escape Enter Tab BTab Up Down Left Right Space BSpace C-c 0`–`9`; refused while it waits for a message |

Input is typed into the agent's pane as if at its keyboard, so it is the
user's prompt: it resets a blocked feature, and the conversation shows it as
`user`. An agent between turns waiting in pm's Stop hook gets it too:
Claude Code and codex hold what is typed there, so the hook lets the turn
end for them to submit it. An `input` the agent can't take now is refused
with `409` and `{"error", "refused"}`: `asking` (a dialog is up; answer it
with `keys`), `not-at-prompt` (a draft in its input line, or no input line
on screen), `not-running`, `no-window`, `inactive`, or for `interrupt` and
`keys`, `idle` (between turns, where a key would only end pm's Stop hook). Text is
never merged into a draft typed at the Mac.

#### Transcript contract (version 1)

The conversation of the agent's current session, read from its harness's
own store (Claude Code's and codex's transcript files, opencode's
database) and normalized. A transcript page:

```json
{"version": 1, "harness": "claude-code", "items": [Item, …], "before": "…" | null, "after": "…"}
```

`items` run oldest first. `before` is the cursor of the next older page
(`null` at the conversation's start); `after` is where this read ended.
Cursors are opaque strings. A page holds about `limit` items: a few more
when one record gives several, fewer — possibly none — when a stretch of
the transcript is bookkeeping, so a client pages until `before` is `null`. An `Item` is `{"id", "at": RFC 3339 | null,
"kind", …}`:

| `kind` | Fields | Is |
|---|---|---|
| `user` | `text` | a prompt the human typed |
| `assistant` | `text` | the agent's reply, Markdown |
| `thinking` | `text` | the model's reasoning, where the harness records it |
| `tool` | `name`, `input`, `result` | a tool call: `input` is one line saying what it does; `result` is `null` until it returns, then `{"text", "error", "truncated", "full"?}`, `text` cut at 4 KB, `full` (only when cut) the `ref` that reads the whole |
| `continuation` | `text` | a prompt pm's never-idle loop sent the agent (a wake-up), not the human |
| `compaction` | `summary` (or `null`) | the harness compacted the conversation's context |
| `event` | `text` | anything else worth a row: an interrupt, a failed turn, a background task's end |

An `id` is stable. An item sent again with an id already shown replaces
it: a tool call is sent again once its result arrives, and an opencode
message again while it is written. A client ignores a kind it does not
know. A subagent's conversation is not included.

A watching event stream polls the conversation about every second and sends

```json
{"project", "scope", "agent", "reset": false, "items": [Item, …], "after": "…"}
```

with what changed since `after` (the start of the watch when absent; pass
a page's `after` so nothing between the page and the watch is missed).
When the agent's session changes (restart, fork), it sends `"reset": true`
with the new conversation's latest page and its `before`: the client
replaces what it shows. Claude Code deletes transcripts after 30 days by
default; an agent whose transcript is gone has no conversation (404).

#### The Android app

The app lives in `android/` (Kotlin, Jetpack Compose). Building needs JDK
17+ and the Android SDK with platform 37. The build finds the SDK through
`ANDROID_HOME` or `sdk.dir` in `android/local.properties`; with neither, it
writes the latter for `~/Library/Android/sdk`, where Android Studio installs
it on macOS.

Build the release APK and sideload it:

```sh
android/gradlew -p android assembleRelease
adb install -r android/app/build/outputs/apk/release/app-release.apk
```

The release build is shrunk by R8 and carries only `arm64-v8a` code (the
debug build adds `x86_64` for an emulator). It is signed only when the
Gradle property `pmReleaseSigning` (in `~/.gradle/gradle.properties`, or
`ORG_GRADLE_PROJECT_pmReleaseSigning`) names a properties file with
`storeFile`, `storePassword`, `keyAlias` and `keyPassword`; otherwise it
builds `app-release-unsigned.apk`, which a phone refuses. Keep that file
and the keystore out of the repo, and back both up: an APK signed with
another key installs only after an uninstall, which drops the app's
pairing. For the same reason, installing the release build over the debug
one (`assembleDebug`, signed with the SDK's debug key) needs one
`adb uninstall dev.pm.app` and a new pairing. The version code is the
minute of the last commit, so a build of an earlier commit cannot replace a
later one.

Without `adb`, copy the APK to the phone and open it, allowing installs
from that source. In the app, scan the code `pm serve pair` prints, or
paste its `url`, `device` and `token` lines. It asks to post
notifications. Each time it opens and reaches the server, it registers
through the UnifiedPush distributor it used before — or, if that one is
gone, the phone's default, else any installed one, else Google's — and
sends the server its subscription; Settings switches between them. For
notifications off the tailnet without Google, install ntfy from F-Droid
(its default server is ntfy.sh) before pairing.

The app opens on what needs you: every scope across
projects whose attention isn't `none`, ranked by kind as the attention
view ranks it, then the longest quiet first; a row opens the agent its
attention names, else the scope. Below come the projects, most urgent
first, then a project's `main` and features, then a scope's agents, each
marked with the glyphs and colours of the tmux badges ([tmux
integration](#tmux-integration)); an agent's conversation, from its
harness's transcript, and its screen; a feature's summary. The
conversation takes a message to the agent, and
shows whether it was queued, sent and seen; a busy agent can be
interrupted; the screen has keys for answering a dialog, which the message
box can't. Notifications
come on a channel per kind (needs input, ready for review, agent died),
each tuned in Android's settings; a tapped one opens the scope, or the
agent it names, and opening the app withdraws those a snapshot shows are
over. One naming an agent blocked on you or ready for review takes a reply
inline; a reply that couldn't be sent shows why, with its text.

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
then. Codex and opencode agents block every turn. It also lets the turn
end for text sent from the phone app that Claude Code or codex holds
behind the hook ([Remote access](#remote-access)). The wait has a one-year
timeout, so it never times out in practice.

An agent whose turn ends any other way never re-enters the hook, so a
message to it waits until something prompts it: an interrupt, a rejected
dialog, an API error, or the hook itself ended by its harness (Esc while it
waits), which then says why in the harness's transcript. pm shows such an
agent as `unarmed`, and [`pm msg send`](#messaging) re-arms it when it can.
No hook reports a Claude Code agent interrupted mid-turn or a dialog it
rejected, nor a codex turn an API error ended; pm reads those from the tail
of the session's transcript instead.
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
tracks, nor restores one it deleted; a skill the branch deleted also loses
its harness projection. A feature's own `.agents/skills/` is
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
config, notices, and your global workflows. Both can be git-backed. Your
code travels through each repo's own remote, and agents' conversations
through a `pm harness export` tarball.

`pm migrate check [--project <name>…]` says what a move would lose or fail
on: unpushed branches (a feature branch with no commits of its own needs
no push: `pm restore` creates it from the feature's base) and uncommitted
work in any worktree, state repos
without a remote or with changes not pushed, registry entries `pm restore`
can't clone or pull from, agents still running, and the machine-local
things to redo by hand (harness installs and logins, `pm serve` devices,
your tmux config, your own global skills), with the plan that clears it.
A feature with active agents and uncommitted work is marked in flight:
finishing and merging it before the move beats committing work in
progress. It only reads, and exits non-zero while anything blocks.

On the old machine, once, each against a new, empty repo:

```sh
pm state init --global --remote <registry-url>   # the registry
pm state init --remote <state-url>               # in each project (or `pm state remote` for an existing .pm/ repo)
pm state backfill                                # record repo and state URLs in the registry
```

Then, at each move, from a shell outside pm's tmux sessions:

```sh
pm close --all                  # stop agents; they stay active, so the new machine resumes them
pm state push                   # in each project, then:
pm state push --global
pm migrate check --project <name>…   # until it passes
pm harness export --all --harness <h> -o pm-<h>.tar.gz   # once per harness your agents use
```

On the new machine, with pm (`cargo install --path .`), git with access to
your remotes, `gh`, tmux and your harnesses installed:

```sh
pm state init --global --remote <registry-url>
pm restore --project <name>… --import pm-claude-code.tar.gz   # clone, pull state, recreate worktrees, import, then start agents
```

`pm state init --global --remote` is safe to repeat: one that can't fetch
leaves nothing behind, and with the registry already on that remote it
pulls again. Where it can't fast-forward — this machine registered
projects too — it takes the remote's registry and keeps the projects only
this machine has; one the remote also has is set aside in the config dir's
`registry-before-pull/`. `pm restore` without `--project` restores every
registered project. It starts agents only after the `--import` tarballs
are in, so each resumes its conversation, and lists last any worktree
whose sessions it could not import; `pm harness import <tarball>` imports
one later, into projects already restored, and infers the harness from the
tarball. An import adds only what the machine lacks, never replacing a
session or memory file it has, and rewrites the recorded paths when the
home directory differs. `pm harness migrate --from <old path>` does the
same for a project moved on one machine.

The registry repo syncs your global custom workflows but never the bundled
ones or machine-local files (the harness probe cache, tmux lock files): its
`.gitignore` carries a block pm regenerates, so `pm upgrade` rewriting them
never dirties it. If an earlier release committed any of them, `pm upgrade`
untracks them and stages the deletion for `pm state push --global`. A
machine that pulls that commit loses the bundled dirs until it runs `pm
upgrade`.

## Reference

### tmux integration

Plugin options, set before `run-shell 'pm tmux init'`:

| Option | Default | Effect |
|---|---|---|
| `@pm-bin` | `pm` | the pm binary tmux runs |
| `@pm-auto-refresh` | on | keep pm's options current with a background `pm tmux refresh` loop; pm pushes its own changes at once, so the loop only catches what happens outside pm. The loop also re-sets pm's formats when it starts or pm is upgraded, so a new pm reaches a running server without a config reload; it switches to another pm when `@pm-bin` or the server's `PATH` comes to name one |
| `@pm-refresh-interval` | `30` | seconds between refreshes |
| `@pm-window-status` | on | put each agent window's badge just before the window name in `window-status-format` and `window-status-current-format`, keeping your theme's style for the name |
| `@pm-bind-tree` | on | turn prefix `s` / `w` into pm's tree, sorted by name, when they run tmux's default `choose-tree` |
| `@pm-attention-key` | `a` | the prefix key opening pm's tree with only the sessions needing attention, or a message when none does; `off` for none. A key your config binds is left alone; a key pm lets go of gets tmux's default binding back, if it has one |

Badges are Nerd Font glyphs: an agent window's shows its [agent
state](#attention-view), a feature session's the attention it needs. A
kind that means what a state means shares its glyph. The window list shows
glyphs only; pm's tree, which has room, labels each one: a session line
reads `<glyph> blocked  implementer: which DB?` (on `main`, its main
agent's state), a window line `<glyph> idle <envelope> 2`, and a session's
activity `<gear> working`, `<spinner> background 1d` or `quiet 2h`.
Follow a badge with a space in your own formats: some terminals (Ghostty)
draw a glyph small when the next cell isn't blank.

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
| | `@pm_attention` | the attention kind; unset for `none`, and for a `ready` feature while an agent is busy |
| | `@pm_reason` | the attention detail, or for `stalled` what the attention view shows; unset without one |
| | `@pm_badge` | the kind's glyph, styled; unset for `none`; on `main`, its main agent's badge |
| | `@pm_label` | `@pm_badge` with words, as pm's tree shows it: the kind after its glyph; on `main`, its main agent's `@pm_agent_label` |
| | `@pm_activity` | the busy glyph while the scope is working, else the background glyph and how long its oldest background wait has run (`1d`), else how long it has been quiet (`2h`, styled); unset under 10 minutes quiet, and on `main` while its badge already shows its main agent busy |
| | `@pm_activity_label` | `@pm_activity` with words, as pm's tree shows it: `working` or `background 1d` after the glyph, or `quiet 2h`; unset when it is |
| | `@pm_alert_pending`, `@pm_alerted` | pm's own bookkeeping: a ready alert waiting for its team to go quiet, and the kinds already alerted on |
| agent window | `@pm_agent` | the agent's name |
| | `@pm_agent_state` | an [agent state](#attention-view) |
| | `@pm_unread` | unread message count |
| | `@pm_agent_badge` | the badge, styled; it resets with `#[default]`, so placed anywhere but the start of a format, follow it with your theme's style |
| | `@pm_agent_label` | the badge with words, as pm's tree shows it: the state after its glyph, the unread count after the envelope |
| global | `@pm_summary` | each kind's glyph and how many sessions have `@pm_attention`, styled and joined by ` · `; unset when none has |
| | `@pm_count` | sessions with `@pm_attention` set, so it matches what `@pm-attention-key` opens; a feature whose session is closed is not counted (`pm status` lists it) |
| | `@pm_features_alerted` | pm's own bookkeeping: the kinds each feature has alerted on, kept for a closed feature |
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
| `ready` | status `ready`, or PR `approved`; while an agent is busy its session keeps the badge but publishes no `@pm_attention` (so it is not counted), and its alert waits until none is | the summary's first line, or `PR approved` |
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
rejection. A scope is working while a busy agent showed activity in the
last 20 minutes. Background work is not working: a scope with a
`background` agent shows how long its oldest has waited (`background 1d`),
since only the job's end wakes it, and pm can't tell a stuck job from a
long one. Otherwise rows show how long it has been quiet. PR state is what
`pm feat sync` last recorded: the view never calls GitHub, so it is cheap
to poll.

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
      "background_since": null,
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
      "waiting": {
        "kind": "question",
        "detail": "Postgres or SQLite?",
        "since": "2026-10-02T09:31:00Z"
      }
    }],
    "working": false,
    "background_since": null,
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
`error`, `prompt`, `tripped`, `background`), else `null`; its `since` is
when that began, `null` for `tripped`. `background_since` is when the
scope's longest-waiting `background` agent began waiting, `null` with none.
`last_activity` is the last time any of the scope's agents showed
activity, `null` if none ever has.

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
- **Typing into an idle agent queues the text.** While the agent waits in
  the Stop hook, codex holds what you type as "Messages to be submitted
  after next tool call" and submits it only once you press Esc. No hook
  reports queued input, so pm can't see it until then.
- **Removing a model row keeps the session's model.** `codex resume` with
  no `-m` reuses the model the session last ran, so deleting an
  `[agents.models]` row changes nothing on restart; set the row to the
  model you want instead.
- `pm harness probe --harness codex` checks the version (0.156.0 or newer).

### opencode agents

Set `[agents.harness] <def> = "opencode"` and pm spawns that agent in the
opencode TUI (2.0.18 or later; `pm harness probe --harness opencode`
checks). What differs:

- **The loop is a plugin**, `pm-never-idle`, which pm installs under
  `~/.config/opencode/plugins/`. It stops itself after five turns in a row
  that read no message (a failing model, an unreadable inbox) rather than
  run away; `pm doctor` reports it with the last error. Fix the cause,
  then `pm agent restart <name>`. A failed turn reads `unarmed` with its
  error for the 30 s the plugin waits before it asks again. A turn the
  plugin didn't prompt — one you typed, or one opencode started itself —
  ends its wait, and the turn's end starts the next one.
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
