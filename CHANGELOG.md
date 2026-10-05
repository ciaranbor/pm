# Changelog

pm and its Android app share one version. `scripts/release` rolls the
Unreleased section into the release's own, which becomes its GitHub release
notes.

## Unreleased

- The definition-less vanilla agent is now `plain` (it was `default`,
which read like a catch-all in `[agents.*]`, where that is `"*"`).
`default` is an ordinary name. `pm upgrade` keeps an agent already
running as `default` under that name and relaunches it as `plain`.
Rename `default` to `plain` in your own workflows and `[agents.*]` rows;
`pm doctor` reports rows still keyed `default`.

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
