# msg-read-oldest-sender

Bare `pm msg read` with several senders unread now reads the oldest sender's
next message and ends with `N more senders pending: b, c — pm msg read --from
b` instead of erroring. One sender per read is unchanged.

## Decisions that diverged from the brief

- The brief said a read returns "all of that sender's unread messages, as
  today". Today's read returns *one* message and advances the cursor by one;
  that was kept. The footer therefore names other senders only — a same-sender
  backlog is still discovered by the next read or `pm msg list`.
- The footer also appears after `--from <x>` (naming the other unread
  senders), and after "No new messages from x" when others are waiting. The
  rule is one footer rule: "senders with unread messages other than the one
  addressed", not "only on bare reads".
- Stop hook reason now names the senders, oldest first (`You have new messages
  from a, b. …`). It costs one extra inbox scan after the wait returns and a
  few characters in the single-sender case.

## Durable gotchas

- "Oldest" is the timestamp of the sender's *earliest unread* message
  (cursor + 1), not their latest. A message whose `.meta` is missing or
  unparseable (pre-metadata, or `send` caught between writing body and meta)
  is undated and sorts before every dated one; ties break on sender name.
  Only `resolve_sender` reads meta — `check()` must stay meta-free, since it
  is the Stop hook's poll and an Err there lets the agent go idle.
- Timestamps come from `Utc::now()` at send time; two sends in the same
  instant fall back to name order. Tests sleep 2 ms between sends.
