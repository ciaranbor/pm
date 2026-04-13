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

This creates a git branch, worktree, and tmux session. A Claude Code
session is automatically started in the feature's tmux session and the
`--context` text is delivered to the default agent as its first message.

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

You may be working alongside other agents (reviewer, implementer, etc.) on the same feature. Use these commands to communicate:

```sh
pm msg send <agent> "message"         # send a message to another agent
pm msg wait                           # block until a new message arrives
pm msg wait --from <sender>           # block only on a specific sender
pm msg list                           # enumerate your inbox with cursor markers
pm msg list --from <sender>           # scope to one sender
pm msg read                           # print the next unread message (pure)
pm msg read --from <sender>           # scope to one sender
pm msg read --from <sender> --index 3 # dump message 3 absolutely (pure)
pm msg read --from <sender> --index +2  # peek: two past the cursor
pm msg read --from <sender> --index -1  # re-read the one you just processed
pm msg next                           # advance the cursor past the current message
pm msg next --from <sender>           # scope to one sender
pm agent list                         # list agents in the current feature
```

`pm msg read` never advances the cursor — call it as many times as you like.
Once you've actually processed a message, call `pm msg next` to move the
cursor forward by one so the next `pm msg wait` / `pm msg read` moves on to
the following message.

`--from` is required only when the inbox is ambiguous — if only one sender
has unread messages, `pm msg read` / `pm msg next` infer it. `--index`
always requires an explicit `--from`. Past messages stay on disk: use
`pm msg list` to find an index and `pm msg read --from <s> --index <n>` to
revisit any message at any time.

Guidelines:

- Check your inbox (`pm msg list`) between tasks — other agents may have sent you messages
- When you finish a piece of work that another agent needs to know about, send them a message
- Keep messages concise and actionable
- When sending review findings, be specific about files and line numbers

## What NOT to do

- Do not run `pm feat merge`, `pm feat delete`, `pm delete`, or `pm doctor --fix`
- Do not run `pm feat pr` or `pm feat ready`
- Do not run `pm agent spawn` — only the user spawns agents
- These are user-initiated operations — only create features, inspect status, and communicate with other agents via `pm msg`
