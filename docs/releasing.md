# Releasing

pm and the app share one version, Cargo.toml's. Note each user-facing
change under `## Unreleased` in `CHANGELOG.md` as it lands. On an
up-to-date `main`, `scripts/release 0.2.0` bumps the version, makes the
Unreleased notes the release's, commits, tags `v0.2.0` and pushes; CI
(`.github/workflows/release.yml`) builds the binaries and the signed APK
and publishes the release with those notes. `--dry-run` checks everything
first. A version such as `0.2.0-rc.1` is published as a prerelease, which
the install script, `pm self-update` and the app all pass over. CI signs
the APK with secrets that `scripts/release --setup-secrets` sets from
`~/.config/pm-secrets/`, once.
