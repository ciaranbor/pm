---
name: pm-workflow
description: Discover your role and routing within the active feature workflow — who you hand off to and who you report back to. Use at the start of every task, and whenever you are about to hand off, route, or report work to another agent or the user.
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
the user, respond in your own session — no `pm msg` needed; don't report
progress or completion back to `main` unless you're asked to.

## summary.md

If the active workflow names you the summary owner, keep `summary.md` in
the worktree root brief and high signal-to-noise — just what the
orchestrator needs to triage, plus any succinct out-of-scope bugs/ideas.
No exhaustive change logs or manual-test walkthroughs unless they carry
durable signal. It's collected when the feature is merged or deleted.
