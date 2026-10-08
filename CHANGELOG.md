# Changelog

pm and its Android app share one version. `scripts/release` rolls the
Unreleased section into the release's own, which becomes its GitHub release
notes.

## Unreleased

## 0.6.0 — 2026-10-08

- pm's global files follow XDG on macOS as on Linux: settings and the
project list in `~/.config/pm`, machine-only state (phone pairing, the push
key) in `~/.local/state/pm`, the cache in `~/.cache/pm`. Nothing is moved
for you: on macOS, move `~/Library/Application Support/pm` to `~/.config/pm`
before upgrading. If you set `XDG_*` variables, set them in your login
profile so the shell, tmux and launchd agree.
- `main` can mark itself blocked on you (`pm feat status blocked -m …`), so
a project waiting on your decision shows in the attention list, tmux and the
app like a blocked feature. `main` is no longer a valid feature name.
- The Android app:
  - A feature's screen has tabs only for its agents; status, summary, brief,
  details and Merge moved to a feature page behind ⓘ.
  - Merge is disabled, with the reason, unless the work is committed and
  the branch contains its base.
  - A Docs page reads the project's information store (todo, issues,
  findings…).
  - Projects not on this machine are left out.
- Registered projects that aren't on this machine are handled throughout:
commands that name one point at `pm restore`, all-project commands skip it,
`pm restore` finishes a folder holding only pm state, and `pm delete`
unregisters one.
- `pm upgrade` always upgrades every registered project, as `--all` did:
the assets and hooks it installs are shared by all of them, so upgrading
one left the rest half-upgraded. `--all` is still accepted, and ignored.
- Claude Code agents start without a "Stand by." turn, and an agent that
Claude Code compacts while idle no longer reads as busy. A resumed agent
with nothing unread is still told to carry on.
- CI runs the full test suite on Linux too; several Linux-only issues are
fixed, including `pm self-update` on a busy binary.
- Removed: the hidden `pm claude` alias.

## 0.5.1 — 2026-10-08

- The Android app opens, closes and deletes projects; projects with nothing
running are listed under "Closed projects".
- Phone alerts go away once their need is over, even with the app closed,
and an alert waits 10 s so one handled at the terminal never reaches the
phone.
- App polish: one or two actions show as icon buttons instead of a near-empty
⋮ menu; `main` is listed like any other session; long paths keep their
filename (`…/dir/file.txt`), in `pm` status lines too; drafts survive Android
closing the app; error pages recover on reconnect; plus many smaller fixes.
- `pm upgrade` restarts busy agents once they go idle instead of skipping them,
and a single-project upgrade names the other projects it left with outdated
agents. `pm harness export --all` takes `--project`, and `pm migrate check`
judges work in flight from git, not from running agents.
- Fixes: opencode agents no longer stop hearing messages after an hour idle;
`pm delete` no longer leaves a `.pm` folder behind, nor prints "Deleted"
after you answer "n"; `nohup pm serve` survives its terminal closing; `pm
agent restart` no longer calls agents that didn't come up "failed".
- pm needs tmux 3.6 or later and builds with Rust 1.91 or later.

## 0.5.0 — 2026-10-07

- The Android app is redesigned:
- **Home and workspaces:** one home screen (Needs you, Working, Projects) and a workspace per feature
with a tab per agent plus Summary, Brief and Details.
- **Chat:** groups tool calls into collapsible runs and has a full-screen output reader.
- **Prompts:** permission prompts, plans and questions are compact cards; several open prompts page
through; a lone question answers in one tap.
- **Composer:** queued and failed messages show as bubbles with Retry, and drafts are kept per agent.
- **Terminal sheet:** draws the pane cell for cell, with zoom, a sticky Ctrl key and Copy link.
- **Notifications:** say what is asked, group per project, and allow or deny a permission prompt from
the shade on an unlocked phone.
- **Elsewhere:** a reworked notes editor, Settings and pairing, connection and freshness indicators, and
a new icon.
- From the phone: merge, delete and restart work for any paired device;
codex agents' questions show as asking and can be answered; "Forget this
server" unpairs the phone on the server, and `pm serve revoke` cuts a
phone off at once.
- `pm upgrade` restarts idle agents whose launch inputs changed (opt out
with `[upgrade] restart_agents = false`); a launch counts only once the
agent's session has started, and on macOS a hung login keychain holds
restarts back.
- `[bundled]` in the global config disables built-in agents, workflows,
skills or the baseline.
- Agents no longer stay "asking" after a question is answered at the
terminal; messages pm sends itself say there is no one to reply to.
- Fixes: a feature whose cleanup failed can be removed; `pm init` and
`pm register` refuse a taken name and clean up after a failed clone;
read-only codex agents start; phone input is never merged into an
opencode draft; `pm msg send` revives an agent whose tool exited.
- opencode 2.0.23 or later is required.
- pm is MIT-licensed.
- The release APK is named for its version, `pm-<version>-android-<abi>.apk`,
so a second download no longer saves as `pm-android-arm64-v8a(1).apk`. The
unversioned copy stays for a while so 0.4.0 apps still see the update.

## 0.4.0 — 2026-10-06

- Agents are woken through each harness's own input path instead of a
Stop hook that waits inside the turn: Claude Code's `asyncRewake`,
codex's `codex queue`, opencode's plugin as before. Typing, Esc and
keys act at once in an agent's window — no more Esc to type into a
waiting agent — and a Claude Code agent with a background task still
running takes a message straight away. A loop that wakes an agent five
times without it reading anything stops and says so. After upgrading,
restart codex agents; codex asks once to trust the changed hook.
- The definition-less vanilla agent is now `plain` (it was `default`,
which read like a catch-all in `[agents.*]`, where that is `"*"`).
`default` is an ordinary name. `pm upgrade` keeps an agent already
running as `default` under that name and relaunches it as `plain`.
Rename `default` to `plain` in your own workflows and `[agents.*]` rows;
`pm doctor` reports rows still keyed `default`.
- `pm agent restart --all [--global]` restarts every idle agent of a
scope, or of every project; busy, asking and background agents are
skipped and listed.
- `pm notes [project]` edits a per-project notes file in `$EDITOR`; the
Android app reads and edits the same notes, and refuses to overwrite a
newer version.
- From the Android app: merge and delete a feature, restart an agent,
and "Show the terminal" for a dialog no hook can answer, with a key
grid and a text line.
- A feature shows ready only while none of its agents is busy.
- Input from the phone works while the agent's pane shows the tmux tree
or copy mode, and `pm msg send` no longer types into a pane you are
using.
- The app tells "update the app" from "update pm on the server" by the
server's answer, never by comparing versions.

## 0.3.0 — 2026-10-05

- Answer an agent's dialogs from the Android app: questions, permission
prompts and plan approval show as cards, and the answer goes back
through the harness's own hook, so whichever of the phone or the
terminal answers first wins. Works for Claude Code, and for opencode
with `[harness.opencode] auto = false`. `pm upgrade` installs the new
Claude Code hook. The app's Screen tab is gone.
- The app's feature page opens full Summary, Brief and Details screens,
with selectable text and a Copy button. `pm feat info` always shows the
base branch.
- `pm tmux init` starts `status-right` with what needs you, and pm's
announcements appear in the middle of the status line instead of
covering all of it. `@pm-status-right off` / `@pm-status-format off`
opt out.
- The README is shorter; reference material moved to `docs/`.


## 0.2.0 — 2026-10-05

- pm is released on GitHub: install it with the install script, and `pm
  self-update` installs the latest release over itself.
- `pm --version`; `pm doctor` and `pm serve status` show the version, and
  warn when the running server's differs.
- The Android app is published with each release and checks for its own
  updates. ZXing replaces ML Kit for scanning the pairing code. With no
  UnifiedPush distributor installed and no Google push service, the app polls
  the server for notifications.
- `pm self-update` installs the latest release rather than rebuilding pm's
  registered source, and leaves a build from source alone unless `--force`.
