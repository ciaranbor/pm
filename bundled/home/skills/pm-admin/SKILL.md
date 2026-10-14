---
name: pm-admin
description: Choose the pm command for machine-wide administration — setting up or cloning a project (pm init, pm register), reading or changing pm settings (pm config), onboarding the user (harness, model, permissions, hook trust, tmux, phone pairing), and acting on any project's agents (restart, stop, spawn, fork). Use before running any of these.
---

# pm administration

Which command fits which case. Each command's `--help` has its flags;
https://github.com/ciaranbor/pm (README) and its `docs/` explain the
concepts.

## Project setup

| The user has | Run |
|---|---|
| a remote URL and no local copy | `pm init --git <url> [path]` — never `git clone` then `pm register` |
| nothing yet: a new repo | `pm init <path>` |
| an existing local clone | `pm register <path>`: symlinked, so the repo stays where it is; `--move` restructures it in place |

Each gives a project root holding the repo as `main/`, the `.pm/` state
directory, and later the feature worktrees as siblings. Each spawns the
project's `main` unless `--no-main`.

## Settings

`pm config get/set/unset/list`, with `--global` or `--project <P>`. An
unknown key is refused with the valid ones listed; `pm config get` says
which file or row a value comes from. What each key means is in the README's
Configuration section (https://github.com/ciaranbor/pm#configuration) and
`docs/harnesses.md`.

`agents.*` rows are keyed by agent definition, not display name.

## Onboarding order

1. Per role: harness, then model, then permission mode. Set a model in the
   same file as its harness row, since a model row applies only to the
   harness configured beside or below it (README "Models and permissions").
2. If codex is used and `pm config get --global
   harness.codex.trust_pm_hooks` is unset, ask the user whether pm may
   trust its own hooks in codex, now and after each pm update. On yes:
   `pm config set --global harness.codex.trust_pm_hooks true`, then `pm
   harness hooks install`; `pm doctor` confirms.
3. `run-shell 'pm tmux init'` in the user's tmux config.
4. Phone access: `pm serve install --pair <device>`.
5. Bring their projects under pm (Project setup above).

## Acting on agents

Always name the target with `--project <P>` and `--scope <S>`.

- `pm agent restart` picks up config edits or unsticks an agent.
- `pm agent stop`, not `delete`: delete is irreversible, so only when the
  user names it.
- `pm agent spawn` always with `--context`.
- `pm agent fork` only when asked.
- Features in another project go through that project's main (your
  definition's Boundary).
