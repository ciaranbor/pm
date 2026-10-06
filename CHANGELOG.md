# Changelog

pm and its Android app share one version. `scripts/release` rolls the
Unreleased section into the release's own, which becomes its GitHub release
notes.

## Unreleased

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
