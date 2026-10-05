# Changelog

pm and its Android app share one version. `scripts/release` rolls the
Unreleased section into the release's own, which becomes its GitHub release
notes.

## Unreleased

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
