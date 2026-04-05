---
name: pm
description: Dispatch features, send/check/read messages, and communicate with other agents using the pm project manager
---

# pm — Feature Dispatch & Agent Communication

This user manages projects with `pm`, a terminal-based project manager built around git worktrees and tmux sessions. Use this skill to create features, inspect status, and communicate with other agents. **Do not merge, delete, or perform project-level operations** — those are user-initiated only.

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

## Agent messaging

You may be working alongside other agents (reviewer, implementer, etc.) on the same feature. Use these commands to communicate:

```sh
pm agent send <agent> "message"   # send a message to another agent
pm agent check                    # check your inbox for new messages
pm agent read                     # read new messages (marks them as read)
pm agent read --from <sender>     # read only from a specific sender
pm agent list                     # list agents in the current feature
```

Guidelines:
- Check your inbox (`pm agent check`) between tasks — other agents may have sent you messages
- When you finish a piece of work that another agent needs to know about, send them a message
- Keep messages concise and actionable
- When sending review findings, be specific about files and line numbers
- If you need the user's attention, send a message to them (their name appears in `pm agent list`)

## What NOT to do

- Do not run `pm feat merge`, `pm feat delete`, `pm delete`, or `pm doctor --fix`
- Do not run `pm feat pr` or `pm feat ready`
- Do not run `pm agent spawn` — only the user spawns agents
- These are user-initiated operations — only create features, inspect status, and communicate with other agents
