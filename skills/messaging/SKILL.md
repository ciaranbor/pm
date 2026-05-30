---
name: messaging
description: Send, reply to, read, list, and wait for messages between pm agents. Use whenever you need to report results, hand off work, notify, tell, update, or reply to another agent, scope, or the orchestrator — and whenever you run any pm msg command.
user-invocable: false
---

# Messaging — Inter-Agent Communication

## Sending and replying

Always send and reply with a heredoc redirect and a quoted delimiter:

```sh
pm msg send <agent> <<'EOF'
## Body
Your message here.
EOF
```

Reply to the message you just read (auto-routes back to its sender and
scope from `.last_read`):

```sh
pm msg reply <<'EOF'
Your reply here.
EOF
```

Use `reply` when responding to a message you just read; use `send` when
initiating a new conversation or addressing a specific agent.

## Reading

```sh
pm msg read                            # next unread, advances cursor
pm msg read --from <sender>            # scope to one sender
pm msg read --from <sender> --index 3  # re-read msg 3 (no advance)
pm msg read --from <sender> --index +2 # peek two past cursor (no advance)
pm msg read --from <sender> --index -1 # re-read last processed (no advance)
```

`--from` is required only when the inbox is ambiguous and always required
with `--index`.

## Listing and waiting

```sh
pm msg list                  # your inbox with cursor markers
pm msg list --from <sender>
pm msg wait                  # block until a message arrives
pm msg wait --from <sender>
pm agent list                # agents in the current feature
```

## Cross-scope and cross-project

By default messages go to an agent in your own scope (feature). To reach
another scope or project, address it explicitly:

```sh
pm msg send <agent>@<feature> <<'EOF'            # @ shorthand for --scope ("main" = orchestrator)
...
EOF
pm msg send <agent> --scope <feature> <<'EOF'    # same thing, long form
...
EOF
pm msg send <agent> --upstream <<'EOF'           # parent scope (base branch's feature)
...
EOF
pm msg send <agent> --project <name> <<'EOF'     # another registered project
...
EOF
```

`read`, `list`, and `wait` also take `--scope`. `pm msg reply` derives
scope/project automatically from the last-read message — no flags needed.

> A bare `pm msg send <name>` only targets your own scope (and silently
> spawns a new agent there if none exists). If a recipient lives in
> another scope, address it with `<name>@<scope>`, `--scope`,
> `--upstream`, or `--project` — otherwise you'll misroute.

## Guidelines

- **Never `cd` before running `pm`** — pm auto-detects the project root;
  `cd`-ing triggers permission prompts.
- Keep messages concise and actionable; cite files and line numbers.
