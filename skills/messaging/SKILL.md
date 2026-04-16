---
name: messaging
description: Read, send, and manage inter-agent messages using pm msg commands. Use when checking inbox, reading messages, sending messages to other agents, or waiting for messages.
user-invocable: false
---

# Messaging — Inter-Agent Communication

Use these commands to communicate with other agents (reviewer, implementer, researcher, etc.) on the same feature.

## Reading messages

```sh
pm msg read                           # read next unread message and advance cursor
pm msg read --from <sender>           # scope to one sender
pm msg read --from <sender> --index 3 # re-read message 3 (does not advance)
pm msg read --from <sender> --index +2  # peek: two past the cursor (does not advance)
pm msg read --from <sender> --index -1  # re-read the last processed message (does not advance)
```

`pm msg read` reads the next unread message **and advances the cursor** in
one step. Repeated calls walk through the queue. Use `--index` to re-read
a specific message without advancing.

`--from` is required only when the inbox is ambiguous — if only one sender
has unread messages, `pm msg read` infers it. `--index`
always requires an explicit `--from`. Past messages stay on disk: use
`pm msg list` to find an index and `pm msg read --from <s> --index <n>` to
revisit any message at any time.

## Sending messages

```sh
pm msg send <agent> "message"         # send a message to another agent
```

## Listing and waiting

```sh
pm msg list                           # enumerate your inbox with cursor markers
pm msg list --from <sender>           # scope to one sender
pm msg wait                           # block until a new message arrives
pm msg wait --from <sender>           # block only on a specific sender
pm agent list                         # list agents in the current feature
```

## Guidelines

- Check your inbox (`pm msg list`) between tasks — other agents may have sent you messages
- When you finish a piece of work that another agent needs to know about, send them a message
- Keep messages concise and actionable
- When sending review findings, be specific about files and line numbers
