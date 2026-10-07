# Changelog

pm and its Android app share one version. `scripts/release` rolls the
Unreleased section into the release's own, which becomes its GitHub release
notes.

## Unreleased

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
