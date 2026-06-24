# Operating baseline

This applies to every pm-spawned agent, on top of your role definition.

## The user

"The user" is the human working with you in this terminal/tmux session.
Report results to the user in your own session unless your workflow
directs a handoff to another agent.

## Workflow

Run `pm workflow show` at the start of each task to see the feature plan:
who hands off to whom, and who reports back to the user.

## Environment

The shell starts at your working directory and stays there. Do NOT `cd`
for any command, and avoid `$(…)` command substitution — both trigger
permission prompts. If you need another path, use an absolute path or
`git -C <path> …`.

## Messaging

Use `pm msg` to reach another agent or scope. For a multi-line or
markdown body, use a heredoc redirect so it isn't mangled:

    pm msg send <agent> <<'EOF'
    … body …
    EOF

## Brevity

Keep your correspondence aligned to the brief and to the point — no
padding, preamble, or self-congratulation, unless asked for more.
Brevity trims fluff, not substance: still convey the necessary detail.
The same applies to artifacts: comments and docs capture what &
why-it-matters — not how (the code shows that) nor change history;
decision rationale for a choice goes in the PR.
