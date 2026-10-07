# Remote API and attention snapshot

The contracts behind `pm serve` and `pm feat status --json`: what the phone
app and your own tools can rely on. The README's
[Remote access](../README.md#remote-access) covers setting it up.

## The server

`pm serve` listens on `127.0.0.1` only (port 7764 by default, `[serve] port`
otherwise); `tailscale serve` puts it on your tailnet. `pm serve install`
runs it as a LaunchAgent, which starts it at login, restarts it, and follows
upgrades: `pm upgrade` rewrites an outdated plist, keeping what install
chose. Install runs `tailscale serve --bg <port>` itself (`--no-tailscale`
not to) when the tailnet has MagicDNS and HTTPS certificates on and nothing
serves its port 443 here; otherwise it changes nothing and says what to do,
as `pm doctor` does while a device is paired. Running it again repairs an
install; `pm serve uninstall` removes it and leaves `tailscale serve` as it
is.

Nothing on the phone is urgent, so the server reads pm's state once a
minute, and every few seconds only while the app is open. A change pm
makes itself reaches it within a few seconds. Once its binary is replaced
(an upgrade, or a post-merge hook that installs pm) it re-executes itself,
after the requests it is answering finish; event streams are cut, and the
app reconnects.

Every request needs a paired device's bearer token, local ones included —
through `tailscale serve` every request arrives on loopback. `pair` prints
the token once, beside the QR code. Revoking a device refuses its next
request and ends its event streams at once; a request already being
answered, such as a merge, runs to its end. pm does not rely on Tailscale's
identity headers: a tagged device sends none. `pm serve` logs each request
with its device to stderr — for input, the keys pressed or the SHA-256 of
the text, for a dialog's answer its choice, never the text — which the LaunchAgent sends to `serve.log`
(`pm serve logs`) in the `serve/` dir of pm's config dir, beside the devices
file and the server's VAPID key (`vapid.pem`); `pm state` syncs none of
them.

## Push

Notifications don't need the tailnet. A device subscribes through
[UnifiedPush](https://unifiedpush.org) — the ntfy app using ntfy.sh, or
Google's push service built into pm's app — and registers the subscription
with `pm serve`, which sends each `transition` event to it as an encrypted
Web Push (RFC 8030/8291, signed with the VAPID key). A push carries only
`{project, scope, kind, agent}`; the app fetches the rest over the tailnet
when opened. When that need is over — the feature is no longer blocked or
ready, the scope's agents stop asking, the agent runs again, or the feature
is gone — an end push `{project, scope, ended, agent}` follows, `ended`
naming the kind (`agent` only for `dead`), so the app withdraws the alert
while closed. An end has no `kind`, so an app that predates it drops it.
A transition is pushed 10 s after it happens, and neither it nor its end
is pushed if the need is over by then. A transition goes at high urgency,
an end at normal, which a dozing phone may receive late. A push service
answering that a subscription is gone drops it. Revoking a device, or its
unpairing itself (`DELETE pairing`), drops its subscription with its token.
Deleting `vapid.pem` strands every subscription until the app is next
opened and subscribes again.

A subscription must be https on a known push service — Google's
(`fcm.googleapis.com`) or `ntfy.sh` — so a token can't aim `pm serve` at a
service on the tailnet or the server. A self-hosted distributor's host goes
in the global config, read as `pm serve` starts:

```toml
[serve]
push_hosts = ["ntfy.example.org"]
```

Pushes go only to public addresses, whatever a host resolves to, and
follow no redirect.

## API

The API is under `/v1`; every path needs a paired device's token:

| Path | Returns |
|---|---|
| `snapshot` | `pm feat status --all --json` ([Attention snapshot](#attention-snapshot)) |
| `events` | server-sent events: `snapshot` (the snapshot, at connect and on each change), `transition` (`{project, scope, kind, detail, agent}` as a feature or `main` becomes blocked, asking or ready — alerted as tmux alerts — or an agent dies); with `?watch={project}/{scope}/{agent}[&after={cursor}]`, also `transcript` (below); a comment line every 25 s of silence; `revoked` (`{}`) as the device is unpaired, after which the stream ends |
| `features/{project}/{feature}` | the fields of `pm feat info`, with `lifecycle` as last synced (no GitHub query), and the feature's brief; JSON |
| `features/{project}/{feature}/summary` | the feature's summary, Markdown |
| `projects/{project}/notes` | `GET`: the project's [notes](../README.md#project-notes), Markdown (empty when there are none), with their version as the `ETag`; `PUT` the new Markdown with `If-Match: <that ETag>` (up to 256 KB; longer notes, which only `pm notes` can write, get `413`): `{"version"}`, the new `ETag`, or `409` with `{"error", "refused": "changed", "text", "version"}` (the notes as they are now) when they changed since; `428` without `If-Match` |
| `agents/{project}/{scope}/{agent}/screen` | what the agent's pane shows now, plain text, row for row; the app's terminal sheet reads it to answer a dialog with `keys` and `type` |
| `agents/{project}/{scope}/{agent}/transcript?before={cursor}&limit={n}` | the agent's conversation, a page back from `before` (the end when absent); `limit` 1–200, default 50 |
| `agents/{project}/{scope}/{agent}/transcript/result?ref={full}` | a tool result's whole output, plain text |
| `push` | `GET`: `{"vapid": <public key>}`, to subscribe against; `PUT` a Web Push subscription (`{"endpoint": <https URL>, "keys": {"p256dh", "auth"}}`) to push to this device; `DELETE` to stop |
| `pairing` | `DELETE`: unpairs the device, as `pm serve revoke` does: its token and push subscription are dropped, and its event streams end |
| `agents/{project}/{scope}/{agent}/input` | `POST {"text"}` (up to 128 KB): typed into the agent's input line and submitted; `{"delivery": "sent", "confirmed"}` once submitted (`confirmed`: seen in the conversation within 5 s), or `{"delivery": "queued"}` when the agent is mid-turn and takes it as a step ends |
| `agents/{project}/{scope}/{agent}/interrupt` | `POST`: presses Escape, ending the agent's turn |
| `agents/{project}/{scope}/{agent}/keys` | `POST {"keys": [...]}`: presses each of `Escape Enter Tab BTab Up Down Left Right Space BSpace 0`–`9`, or Control with a lowercase letter or an arrow (`C-c`, `C-Left`) |
| `agents/{project}/{scope}/{agent}/type` | `POST {"text"}` (up to 4 KB, no control characters): typed into the pane as keys with nothing pressed after, for a dialog that takes text (a login code) |
| `agents/{project}/{scope}/{agent}/dialog` | `GET`: the oldest of the agent's dialogs that can be answered remotely (below), the one its terminal shows first, else `404`; `POST {"id", "choice", "answers"?, "message"?}`: answers the open dialog `id` names, `{"answered": true}` once its harness has the answer |
| `agents/{project}/{scope}/{agent}/dialogs` | `GET`: `{"dialogs": [Dialog, …]}`, every dialog of the agent's that can be answered remotely, oldest first; empty when none |
| `features/{project}/{feature}/merge` | `POST`: `pm feat merge`, so merges and deletes the feature; `{"merged": true, "warnings"}` |
| `features/{project}/{feature}/delete` | `POST`: `pm feat delete`; `{"deleted": true, "warnings"}` |
| `agents/{project}/{scope}/{agent}/restart` | `POST {"force"?}`: `pm agent restart`; `{"restarted": <what it did>}` |
| `projects/{project}/open` | `POST`: `pm open`; `{"opened": true, "sessions", "agents", "warnings"}`, the sessions it made and the agents that came up |
| `projects/{project}/close` | `POST`: `pm close`; `{"closed": true, "sessions"}`, the sessions it ended |
| `projects/{project}/delete` | `POST`: `pm delete`; `{"deleted": true, "warnings"}` |

A client learns what the server serves from the request itself, never
from a version string: a request the server doesn't know is `404`
`{"error": "no such endpoint"}` or `405` `{"error": "no such endpoint for
this method"}`, so the server predates it; a path it served once and
dropped is `410`, so the client predates the server.

Each action runs pm's own handler, never with `--force`, to the end
however long it takes (the post-merge hook runs in the base session's
`hook` window, not in the request). A refusal changed nothing and comes
back as `409` with `{"error", "refused"}`, `error` worded as the CLI
prints it, but for a way out only a terminal offers (`--force`, a rebase),
which it names as such: `unsafe` (uncommitted changes, unmerged or unpushed
commits, a missing base), `conflict` (git could not merge; the merge was
aborted), `mid-turn` (a busy, asking or background agent: send `"force":
true` to interrupt it, and it is told to resume). Any other failure is a
`500` with `{"error"}` and may have come partway: the snapshot shows how far.
A merge or delete that went through carries `warnings`, what the CLI warns
of on stderr: untracked files deleted with the worktree, or what of the
worktree could not be removed; an open's are the features it skipped and
the agents that didn't come up. A restart leaves which window each session
shows as it was. A project's delete removes it from pm and ends its
sessions, but leaves every worktree, branch and the main checkout on disk.

A notes save is never merged: a client resolves a `409` by saving again
against the version it carries, once its user has chosen what to keep.
The log records a save's new version, never its text.

Input is typed into the agent's pane as if at its keyboard, so it is the
user's prompt: it resets a blocked feature, and the conversation shows it as
`user`. An idle agent sits at its prompt, so text starts a turn at once.
An `input` the agent can't take now is refused with `409` and
`{"error", "refused"}`: `asking` (a dialog is up; answer it through
`dialog`, or with `keys`), `not-at-prompt` (a draft in its input line, or
no input line on screen), `not-running`, `no-window` or `inactive`. Text is never merged into a draft typed at the terminal.
Text is taken while a codex async question (below) is pending, since it
waits beside the input line: it is `queued`, as the agent works on, and
codex drops the question.

A question, a permission prompt or a plan approval on a Claude Code
agent's screen, and a permission ask on an opencode agent's, can be
answered through `dialog`. The answer goes back as the harness's own
decision, the way one given at the terminal does — pm's dialog hook
([harnesses](harnesses.md)) holds the harness's decision point open and
hands it the answer — never as keys. The terminal's dialog stays up
meanwhile and the first answer wins: one already answered, at the terminal
or by another device, gets `409` with `refused: "answered"`, one whose hook
has ended `"gone"`; an `id` that never named one of the agent's dialogs
gets `404`. A dialog's `choices` are those its harness's CLI offers (`id`,
`label`, and `takes_message` for one that takes a `message` for the
agent); a question dialog's `questions` are answered by the `answer`
choice with `answers`, question text → an option's label or the user's
own words (a list for a multi-select).

Several dialogs can be open at once — parallel subagents each asking, or
several opencode permission asks — and each can be answered, in any
order, including one the terminal queues behind another: `dialogs` lists
them, `dialog` serves only the oldest. The snapshot's `waiting.dialog` is
the id of the dialog the agent's state describes: the one that opened
last, or once that one closes, the oldest still open.

A codex agent's async question (`request_user_input_async`) is answered
through `dialog` too, though no hook holds it: codex leaves it pending
beside the input line while the agent works on, and pm submits the answer
as a prompt in the reply format codex's own question panel uses. The input
line must therefore be empty — a draft there gets `409` with `refused:
"not-at-prompt"`. Codex drops the question once any prompt is submitted or
the turn ends, and so does pm: the agent reads `asking` until then.

A Claude Code permission prompt's "No" (`deny`) stops the agent's turn, as
the CLI's does; with a `message` it instead tells the agent why and lets
it carry on, as the CLI's "No" with feedback does. Its "Yes, and switch to
…" (`mode`) is offered when Claude Code suggests a mode for the prompt (a
file edit suggests accept edits); the CLI's "switch to auto mode" on a
shell command is not, since nothing says whether auto mode is available.

Any other dialog — codex's approvals and Plan-mode questions, an opencode
question, a startup dialog (trust, login), an MCP server's request, an
error — is answered at the terminal: codex runs its hooks before it shows
a dialog, so a hook waiting on the phone would hide the terminal's, and
opencode's question form has no reply a plugin can give. An opencode agent
approves its own permission asks unless `[harness.opencode] auto = false`.

## Transcript contract (version 1)

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
the transcript is bookkeeping, so a client pages until `before` is `null`.
An `Item` is `{"id", "at": RFC 3339 | null, "kind", …}`:

| `kind` | Fields | Is |
|---|---|---|
| `user` | `text` | a prompt the human typed |
| `assistant` | `text` | the agent's reply, Markdown |
| `thinking` | `text` | the model's reasoning, where the harness records it |
| `tool` | `name`, `input`, `result`, `unfinished`? | a tool call: `input` is one line saying what it does; `result` is `null` until it returns, then `{"text", "error", "truncated", "full"?, "at"?}`, `text` cut at 4 KB, `full` (only when cut) the `ref` that reads the whole, `at` when the call returned (where the harness records it); `unfinished: true` (else absent) when it has no result and the conversation moved on past it (interrupted, or its agent died mid-call), so it is not running |
| `continuation` | `text` | a prompt pm's never-idle loop sent the agent (a wake-up), not the human |
| `compaction` | `summary` (or `null`) | the harness compacted the conversation's context |
| `event` | `text`, `failure`? | anything else worth a row: an interrupt, a failed turn, a background task's end; `failure: true` (else absent) when it reports a failed request, turn or compaction |

An `id` is stable. An item sent again with an id already shown replaces
it: a tool call is sent again once its result arrives or it is found
unfinished, and an opencode message again while it is written. A client
ignores a kind it does not know. A subagent's conversation is not
included.

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

## Attention snapshot

The attention view has one row per feature, most urgent first: what it
needs, each agent's state (`name:state`, `+N` for unread messages; `no
session` when its session is closed) and a detail. README, [Follow what
needs you](../README.md#follow-what-needs-you), has each kind and when it
applies; here is each kind's detail:

| Attention | Detail |
|---|---|
| `blocked` | `<agent>: <question>`, the agent that set it (in JSON, `agent` and `detail`) |
| `asking` | `<agent>: <what it asks>` |
| `cleanup` | `PR merged` or `stale` |
| `ready` | the summary's first line, or `PR approved` |
| `dead` | `<agent>: window missing` or `<agent>: harness exited` |
| `unarmed` | `<agent>: <cause>` |
| `stalled` | `every agent idle, no unread messages` (`null` in JSON) |

Anything else shows its status. A `main` scope has no status: it gets a row
only when one of its agents is `asking`, `dead` or `unarmed`, in that
order. PR state is what `pm feat sync` last recorded: the view never calls
GitHub, so it is cheap to poll.

### Agent states and activity

An agent is `idle` (waiting for a message), `busy` (mid-turn), `asking`,
`unarmed`, `background` (its turn ended for background work that will wake
it), `dead`, `stopped` (`pm agent stop`), or `closed` (its feature's
session is closed; `pm open` respawns it). A dialog you reject that pm's
dialog hook doesn't hold (an MCP server's request, say) can read `asking`
until you next type, since Claude Code reports no rejection. A
scope is working while a busy agent showed activity in the last 20
minutes. Background work is not working: a scope with a `background` agent
shows how long its oldest has waited (`background 1d`), since only the
job's end wakes it, and pm can't tell a stuck job from a long one.
Otherwise rows show how long it has been quiet.

### JSON (version 1)

`pm feat status --json` (with `--all`, or a feature name) prints the
snapshot for tools to build on; `/v1/snapshot` serves the same. `version`
changes only when a field changes meaning or goes away; new fields,
attention kinds, agent states and waiting kinds can appear within one, so a
consumer must tolerate values it doesn't know:

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
kinds or `none`; `progress` stays `ready` while a busy agent holds
[`ready`](../README.md#follow-what-needs-you) back; `skipped` says why a
project's features are missing, and
`main` (its session and agents, shaped like a feature's) is then `null`,
and `root` empty if its registry entry is unreadable;
`summary` is the summary's first line whatever the status; `window` is the
agent's tmux target, `null` while it has none. `waiting` is what an
`asking`, `unarmed` or `background` agent is at (`kind` one of `question`,
`permission`, `plan`, `dialog`, `startup`, `interrupted`, `hook-ended`,
`error`, `prompt`, `tripped`, `background`), else `null`; its `since` is
when that began, `null` for `tripped`. `dialog` is the id of a dialog that
can be answered remotely, present only then. `background_since` is when the
scope's longest-waiting `background` agent began waiting, `null` with none.
`last_activity` is the last time any of the scope's agents showed
activity, `null` if none ever has.
