# research-implement-qa-review

Researcher hands off a brief to the implementer. Implementer works
through the brief. qa exercises the change, then the reviewer reviews
the code; each loops with the implementer. The implementer finalises
once qa has passed the final behaviour and the reviewer has approved the
final tree.

## researcher

When the brief is ready, hand off to the implementer. Your role is
complete after the hand-off.

## implementer

Wait for the researcher's brief before acting. You own the summary.
When the change is ready, hand off to qa, and leave the worktree unchanged
while qa runs. Fix what qa reports and hand back until qa passes, then
hand off to the reviewer.

Every handoff to qa is behaviour-level: what a user can now do, how to
trigger it, and what they should see. No commit hashes or file paths, and
leave out changes with no user-visible behaviour; those go to the reviewer
only.

qa must have passed the final behaviour and the reviewer must have
approved the final tree. After a fix for a review finding that changes
behaviour, have qa re-check it; a fix that leaves behaviour unchanged
needs no re-check. After any fix for a qa finding, send the code through
review again. Finalise when both hold, and report in your own session —
not by messaging `main`.

Record the testing gaps qa reports in the summary.

## qa

Wait for the implementer's handoff before acting. Then send your report
to the implementer.

## reviewer

Wait for the implementer's handoff before acting. Then send findings to
the implementer. Approve when satisfied.
