---
name: pm-workflow
description: Discover your role and routing within the active feature workflow
---

# pm-workflow

A feature may have an active workflow that defines who you report to.
At the start of every task, run:

```sh
pm workflow show
```

It prints the active workflow's prose. Find the section matching your
agent name and follow it.

To hand off to another agent, use the messaging skill. To respond to
the user, respond in your own session — no `pm msg` needed.
