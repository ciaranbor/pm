---
name: pm
description: Dispatch features using the pm project manager (worktrees + tmux sessions)
---

# pm — Feature Dispatch

This user manages projects with `pm`, a terminal-based project manager built around git worktrees and tmux sessions. Use this skill to create and inspect features. **Do not merge, delete, or perform project-level operations** — those are user-initiated only.

## Creating a feature

```sh
pm feat new <name> --context "description of the task"
```

This creates a git branch, worktree, tmux session, and seeds a `TASK.md` with the context. A Claude Code session is automatically started in the feature's tmux session to work on the task.

Options:
- `--context <text-or-file>` — seed TASK.md (required for agent-driven features)
- `--base <branch>` — stack on another branch instead of main
- `--name <override>` — override the derived feature name (useful for branches with slashes)
- `--no-edit` — disable auto-accept edits in the spawned Claude session

## Adopting an existing branch

```sh
pm feat adopt <branch> --context "description"
pm feat adopt <branch> --from /old/worktree/path  # migrate Claude sessions
pm feat adopt ciaran/feature --name clean-name
```

Creates a feature from a branch that already exists. Does not create a new branch.

## Checking status

```sh
pm feat list          # list all features with status
pm feat info <name>   # full details for a feature
pm status             # project dashboard
```

## What NOT to do

- Do not run `pm feat merge`, `pm feat delete`, `pm delete`, or `pm doctor --fix`
- Do not run `pm feat pr` or `pm feat ready`
- These are user-initiated operations — only create and inspect features
