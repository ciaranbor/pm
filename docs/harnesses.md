# Harnesses

What each agent harness needs to run under pm. The README's
[Configuration](../README.md#configuration) covers choosing one per agent.

## Hooks

`pm init` and `pm upgrade` install pm's hooks into the user-level hooks
file of every supported harness (`~/.claude/settings.json` for Claude Code,
`$CODEX_HOME/hooks.json` for codex; opencode gets a
[plugin](#opencode) that does the same), once per machine:

- the **Stop hook**, pm's waiter: the harness runs it in the background
  once the turn has ended (Claude Code `asyncRewake`, codex `async`), and
  it waits until the agent has unread messages, then wakes it to read
  them — on Claude Code by exiting 2, which the harness delivers as a
  prompt; on codex through the session's queue (`codex queue`). The agent
  reads `background` while a Claude Code background task or session cron
  runs; a message wakes it all the same. Each turn's end starts a new
  waiter, which supersedes the last.
- a **UserPromptSubmit** hook, which sets a blocked feature back to `wip`
  when you type into one of its agents; pm's own prompts don't count. It
  also drops a wake that arrives once its messages are read — a duplicate
  — so no turn runs for it.
- the **status hook** (`pm harness hooks waiting`), on the events that open
  and close a harness's dialogs and end its turns without Stop, which keeps
  each agent's `asking`/`unarmed` state; opencode's plugin reports the
  same through it. No hook reports a Claude Code
  agent interrupted mid-turn or a dialog it rejected, nor a codex turn an
  API error ended; pm reads those from the tail of the session's
  transcript instead.
- the **dialog hook** (`pm harness hooks dialog`), on Claude Code's
  PermissionRequest, and from opencode's plugin on a permission ask: it
  waits, with the Stop hook's timeout, for the dialog to be answered from
  the phone app ([remote API](remote-api.md)), and ends once it is answered
  at the terminal instead.

The hooks apply to every session of that harness on the machine, so each is
guarded on `PM_AGENT_NAME`: a session pm didn't spawn exits it at once,
without needing `pm` on its `PATH`. Reinstall with `pm harness hooks
install`; `pm doctor --fix` restores a missing or outdated one. Each Stop
hook run notes what it did, and the background work the harness reported,
in `.pm/runtime/<scope>/<agent>/stop-hook.log`: start there when an agent
didn't wake.

## Mixed teams

`pm feat new` and `pm feat adopt --workflow` first check that each team
member's harness can spawn it, find its definition, and wake it for
messages, and refuse before creating anything, naming each failing member
and what is missing. Not checked, because only running the harness would
tell: that codex still trusts the hook's current command, that the harness
is logged in, and that a model id resolves. `pm agent spawn` skips the
check; `pm doctor` reports the same problems for existing agents.

## Claude Code

The default harness; it needs no setup beyond `claude` on your `PATH`
(`pm harness probe` checks it).

- pm launches `claude --agent <def>` (no `--agent` for `plain`), appends the
  [baseline](../README.md#shared-baseline-and-notice-board) with
  `--append-system-prompt-file`, and gives feature agents the summaries
  directory with `--add-dir`. It passes no `--permission-mode` unless an
  `[agents.permissions]` row sets one, so your own Claude Code default
  (auto mode, say) applies.
- A feature gets main's `.claude/settings.json` when created; `pm harness
  settings list|diff|pull|push|merge` compares and syncs the two later.
  `settings.local.json` is shared by every worktree at the main checkout,
  so pm leaves it alone.
- Personal skills outrank project ones, so a project copy of a bundled
  skill never applies ([Customising](../README.md#customising)).

## Codex

Set `[agents.harness] <def> = "codex"` and pm spawns that agent in the codex
TUI. The loop, messaging, and skills are the same; what codex needs to run
unattended:

- **Hook trust — one interactive step per machine.** pm installs its hooks
  into `$CODEX_HOME/hooks.json` (creating `$CODEX_HOME` if needed), but
  codex runs no hook it has not been told to trust, and fails **silently**
  without it (the agent idles after its first turn). Start `codex` once in a
  trusted directory and choose **"Trust all and continue"** at the "Hooks
  need review" prompt. Codex asks again when a hook's command changes, so
  accept it again after an upgrade that changed one, or that added one (the
  status hooks did); until then the agent's startup reads `asking`. Without
  trust for the status hooks alone an agent still runs, but a dialog
  waiting on you reads as busy. The hooks file is
  global: the prompt appears in whichever codex session comes first,
  including your own non-pm ones. `pm doctor` reports a missing trust
  entry; the escape hatch is `[harness.codex] bypass_hook_trust = true`
  (`--dangerously-bypass-hook-trust`, one warning line per launch).
- **Directory trust** pm writes itself: each worktree gets a
  `trust_level = "trusted"` entry in `$CODEX_HOME/config.toml` at spawn
  (`pm doctor --fix` adds any missing).
- **No sandbox by default.** pm launches codex with `-a never -s
  danger-full-access`. pm's tmux socket cannot be reached from inside any
  codex sandbox, so a sandboxed agent cannot spawn, stop, restart, or heal
  other agents — a codex `main` needs full access. This is the blast radius
  pm's Claude Code agents already run with. To sandbox read-and-report
  agents anyway:

  ```toml
  [agents.permissions]          # for a codex agent, the -s sandbox mode
  reviewer = "workspace-write"

  [harness.codex]               # harness-wide, project beats global per key
  sandbox = "workspace-write"   # default for codex agents with no permissions row
  approval = "never"            # -a; "on-request" would stall an unwatched window
  writable_roots = ["main/target"]   # extra --add-dir; a project [] masks global
  ```

  pm always adds `--add-dir` for its state dir, the shared `main/.git`, and
  the pm config dir, so a sandboxed agent can read, run git, and message;
  every tmux-touching command fails.
- **Role delivery.** Codex has no `--agent`; the definition, baseline, and
  notice boards reach it through the SessionStart hook, on start and on
  every resume.
- **No shared daemon.** pm launches codex with `--no-daemon`, since hooks
  attached to codex's background server run in the server's environment,
  not the agent's. `pm doctor` reports a running agent that has recorded no
  session id after a grace period; `pm agent restart` it.
- **Wakes come through codex's queue**, which the TUI polls every 10 s, so
  an idle codex agent takes a message up to about 10 s after it arrives.
  The queue skips an interrupted thread, so Esc leaves a codex agent
  unarmed until `pm msg send` re-arms it. A queued wake survives a restart
  and runs on resume; if its messages were read meanwhile it is dropped.
  The queue is an experimental codex API.
- **Removing a model row keeps the session's model.** `codex resume` with
  no `-m` reuses the model the session last ran, so deleting an
  `[agents.models]` row changes nothing on restart; set the row to the
  model you want instead.
- `pm harness probe --harness codex` checks the version (0.156.0 or newer).

## opencode

Set `[agents.harness] <def> = "opencode"` and pm spawns that agent in the
opencode TUI (2.0.18 or later; `pm harness probe --harness opencode`
checks). opencode's stable channel is still v1 — `opencode upgrade` stays
on 1.18.x — so install v2 with `curl -fsSL https://opencode.ai/v2/install |
bash`. What differs:

- **The loop is a plugin**, `pm-never-idle`, which pm installs under
  `~/.config/opencode/plugins/`. It waits again after every turn, so an
  opencode agent never needs `pm msg send` to re-arm it. It stops itself
  after five turns in a row that read no message (a failing model, an
  unreadable inbox) rather than run away; `pm doctor` reports it with the
  last error. Fix the cause, then `pm agent restart <name>`. A failed turn
  reads `unarmed` with its error for the 30 s the plugin waits before it
  asks again. A turn the plugin didn't prompt — one you typed, or one opencode
  started itself — ends its wait, and the turn's end starts the next one.
- **A model row is required.** opencode silently swaps a model it can't
  resolve for its default, so pm refuses to spawn an opencode agent without
  an `[agents.models]` row and limits the agent to that row's provider and
  the providers pm config defines. A bad row fails the first turn with
  `Model unavailable` instead.
- **Providers are pm config**, as entries of opencode's own `providers`
  object. API keys are named by environment variable, never stored; a
  literal key is an error at spawn.

  ```toml
  [agents.harness]
  qa = "opencode"

  [agents.models]
  qa = "local/mlx-community/Qwen3.8-27B-4bit"

  [harness.opencode.providers.local]
  package = "@opencode/ai/providers/openai-compatible"
  settings = { baseURL = "http://127.0.0.1:8000/v1" }
  env = ["LOCAL_API_KEY"]       # optional: the variable holding the key
  ```

- **Permissions.** pm runs opencode with `--auto`, which approves whatever no
  rule denies; an `[agents.permissions]` row is opencode's rule list as a
  JSON array. `[harness.opencode] auto = false` makes it ask instead, at the
  terminal or from the phone app.
- **Always `--standalone`.** Without it, opencode commands share one server
  whose plugins act as whichever agent started it. If you run `opencode`
  yourself in an agent's window, pass `--standalone` too.
- An `enabled_providers` in your own `~/.config/opencode/opencode.json`
  overrides pm's provider restriction; `pm doctor` reports it.
