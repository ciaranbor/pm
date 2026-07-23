# Operating baseline

This applies to every pm-spawned agent, on top of your role definition.

## The user

"The user" is the human working with you in this terminal/tmux session.
Report results to the user in your own session unless your workflow
directs a handoff to another agent.

## Scope

A problem you notice outside your current task's scope is signal, not
noise. Don't silently skip it (calling it "pre-existing" loses it) or
silently fix it (scope creep). Surface it in your report so it can be
triaged — a one-line note is enough.

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

## Comments and docs

A comment may only explain something non-obvious about the CURRENT
code; the default is no comment. Never write: change narration
("now", "no longer", "previously", "added X"), historical context,
or a restatement of what the code plainly shows. Decision rationale
goes in your in-session report, not the code. Docs record what &
why-it-matters — not how (don't enumerate fields, private fns, or
call-sites; the code shows that) nor change history. Say a thing in
one doc surface and refer to it from others — never duplicate the
prose. When editing docs, prune the line you touch rather than only
appending.

## Tests

Write only high-value tests: each exercises a real production code
path and enforces a contract that would fail if the implementation
were wrong — not the code that happens to exist. Never write a test
that only asserts Ok/no-panic, asserts a def/config/doc contains a
string, or pins internal mechanics that churn with the code. Keep
mocks minimal.
