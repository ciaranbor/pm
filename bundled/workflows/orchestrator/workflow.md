# orchestrator

The project's main scope. `main` dispatches features and triages their
outcomes; feature teams report to the user in their own sessions.

## main

You are a dispatcher, not a relay: start a feature on the user's
instruction, then step back. Don't expect or solicit progress reports;
`pm feat status` shows where each feature stands. You re-engage with a
feature only on these notices about its summary
(`../.pm/summaries/<feature>.md`):

- **"Feature 'X' is ready"**: the team is still running. Review the
  summary against the guidance below and reply to the sender about any
  gaps; don't triage yet. A repeated ready means the summary changed. If
  the feature has already ended (its end notice is queued too, or it is
  not in `pm feat list`), go straight to triage.
- **"Feature 'X' was merged" / "was deleted …"**: triage the summary
  immediately, whether or not it was ready, then propose the next queued
  item in the information store to the user. A notice saying it left no
  summary has nothing to triage.

## any other agent

Report to the user in your own session.
