---
name: pm
description: Create and dispatch features, check feature or project status, adopt branches, and inspect the pm project lifecycle. Use whenever you need to start, dispatch, or delegate work as a feature, or look up status with pm feat, pm status, or related pm commands.
---

# pm — Feature Dispatch & Status

This user manages projects with `pm`, a terminal-based project manager built around git worktrees and tmux sessions. Use this skill to create features and inspect status. **Do not merge, delete, or perform project-level operations** — those are user-initiated only.

## Creating a feature

```sh
pm feat new <name> --context "description of the task"
```

This creates a git branch, worktree, and tmux session. A Claude Code
session is automatically started in the feature's tmux session and the
`--context` text is delivered to the default agent as its first message.

## Choosing a workflow

Before dispatching, run `pm workflow list` to see installed workflows.
Each row carries a `use when:` hint describing the situation it fits —
consult these to pick the workflow that matches how the work should be
sliced, then pass it with `--workflow <name>`. Omit `--workflow` for the
single-agent case: `--context` alone defaults to the `solo` workflow
(one developer owning the feature end-to-end).

Options:

- `--context <text-or-file>` — initial message delivered to the default agent's inbox (required for agent-driven features)
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

## Agent messaging

For messaging commands (`pm msg send`, `pm msg read`, `pm msg list`, `pm msg wait`), see the **messaging** skill.

## What NOT to do

- Do not run `pm feat merge`, `pm feat delete`, `pm delete`, or `pm doctor --fix`
- Do not run `pm feat pr` or `pm feat ready`
- Do not run `pm agent spawn` — only the user spawns agents
- These are user-initiated operations — only create features and inspect status
