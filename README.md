# pm

Terminal-based project manager built around tmux and git worktrees.

## Requirements

- tmux
- git
- [gh](https://cli.github.com/) (for `pm feat pr`, `pm feat review`, `pm feat sync`)

## Install

```sh
cargo install --path .
```

Installs the `pm` binary to `~/.cargo/bin/`. Ensure this is in your `PATH`.

## Quick start

### Option A: New project from scratch

```sh
pm init ~/projects/myapp
cd ~/projects/myapp/main
```

Creates a project root with a git repo in `main/` and a `.pm/` state directory.

### Option B: Register an existing repo (symlink)

```sh
pm register ~/code/myapp --name myapp
cd ~/code/myapp-pm/main
```

Creates a wrapper directory (`myapp-pm/`) with a symlink to the original repo as `main/`. The original repo is untouched.

### Option C: Register an existing repo (move)

```sh
pm register ~/code/myapp --name myapp --move
cd ~/code/myapp/main
```

Restructures the repo in-place: moves it into `main/` within a new wrapper at the same path. No `-pm` suffix needed since the original directory becomes the wrapper.

### Create a feature

```sh
pm feat new login
pm feat new login --context "Implement login page per issue #42"
pm feat new login --context path/to/brief.md
pm feat new child --base parent      # stack on another feature
pm feat new login --context "task description" --no-edit
pm feat new ciaran/login                        # feature name: ciaran-login
pm feat new ciaran/login --feature-name eval    # feature name: eval
```

Creates a git branch, worktree, and tmux session (`myapp/login`). With `--context`, seeds a `TASK.md` in the worktree and spawns a Claude session with auto-accept edits enabled. Use `--no-edit` to disable auto-accept edits. With `--base`, branches from the specified branch instead of the default. When `--base` is omitted, the current branch is detected from CWD — so running `pm feat new child` from within a feature worktree automatically stacks on that feature.

Branch names with slashes are supported — slashes are automatically replaced with dashes for the feature name (used for the worktree directory, state file, and tmux session). Use `--feature-name` to override the derived name.

### List features

```sh
pm feat list
```

Shows all features with status, plus branch (if different from name), base, and PR when set.

### Show feature details

```sh
pm feat info login              # show details for a specific feature
pm feat info                    # show details for current feature (detected from CWD)
```

Displays all state fields: name, status, branch, worktree, base, pr, context, created, last_active. Empty optional fields are omitted.

### Switch to a feature

```sh
pm feat switch login             # direct switch
pm feat switch                   # interactive picker (tmux display-menu)
```

### Merge a feature

```sh
pm feat merge login             # merge and clean up (default)
pm feat merge --keep            # merge but keep the feature (worktree, session, branch)
```

Merges the feature into its base branch (defaults to main; stacked features merge into their parent) and cleans up by default (kills session, removes worktree, deletes branch, removes state). Use `--keep` to preserve the feature after merging. Blocks if either the feature or base worktree has uncommitted changes. Always creates a merge commit (`--no-ff`). Feature name is detected from CWD if omitted. If the branch was already merged (e.g. via GitHub PR), the local merge is skipped. If a merge conflict occurs, the merge is aborted and the base worktree is left clean.

### Lifecycle hooks

Every project is bootstrapped with two hook scripts:

- `.pm/hooks/post-create.sh` — runs in the feature's tmux session after `pm feat new`
- `.pm/hooks/post-merge.sh` — runs in the base branch's tmux session after `pm feat merge`
- `.pm/hooks/restore.sh` — runs in each session when `pm open` recreates it (e.g. after reboot)

Hooks run asynchronously in a dedicated `hook` window via `tmux send-keys` — pm does not block on their completion. The hook window is reused across invocations.

`post-create.sh` and `post-merge.sh` are created with defaults on `pm init`. `restore.sh` is opt-in — create it yourself for lightweight recovery tasks like reopening an editor.

Edit the scripts to add your own logic (install deps, run migrations, deploy, etc.). Removing a hook script disables it.

### Create a PR

```sh
pm feat pr login                # create a draft PR for the feature
pm feat pr --ready              # create a non-draft PR (feature detected from CWD)
```

Pushes the branch to origin, then creates a GitHub PR via `gh`. Draft by default; use `--ready` for a non-draft PR. If a PR already exists for the branch, links it instead of creating a new one. For stacked features, the PR targets the base branch instead of main. Respects `.github/pull_request_template.md` if present. Stores the PR number in feature state. Draft PRs keep `wip` status; `--ready` sets status to `review`. Feature name is detected from CWD if omitted.

### Mark a PR as ready for review

```sh
pm feat ready                   # mark current feature's PR as ready
pm feat ready login             # mark a specific feature's PR as ready
```

Pushes latest commits, then calls `gh pr ready` to remove draft status. Sets feature status to `review`. Fails if the feature has no linked PR — run `pm feat pr` first.

### Review a PR

```sh
pm feat review 42                                    # by PR number
pm feat review https://github.com/owner/repo/pull/42 # by URL
```

Fetches the PR commits, creates a worktree and tmux session (`myapp/review-42`), and seeds a `TASK.md` with the PR title, URL, and body. Opens a Claude session pointed at `TASK.md`. Feature status is set to `review` and the PR is linked automatically. Works for both same-repo and fork PRs. Use `pm feat delete` to clean up when done.

### Sync feature statuses with GitHub

```sh
pm feat sync                    # sync all features with their linked PRs
pm feat sync login              # sync a specific feature
```

Queries GitHub for each feature's PR state and updates the local status: draft PRs set `wip`, ready PRs set `review`, merged PRs set `merged`, closed PRs set `stale`. Reports changes and suggests `pm feat delete` for merged features. Feature name is detected from CWD if omitted; if not in a feature worktree, syncs all features.

### Rename a feature

```sh
pm feat rename login auth        # rename feature "login" to "auth"
pm feat rename new-name          # rename current feature (detected from CWD)
```

Renames the git branch, moves the worktree directory, renames the tmux session, and updates the state file. Blocks if the new name already exists as a feature or branch. Local only — does not touch remote branches or linked PRs.

### Delete a feature

```sh
pm feat delete login             # with safety checks
pm feat delete --force           # delete current feature, skip safety checks
```

Safety checks block deletion if the feature has uncommitted changes or commits not merged into its base branch (defaults to main; stacked features check against their parent). If the feature has a linked PR that was merged on GitHub, the merge and unpushed checks are skipped (handles squash merges) and the post-merge hook is triggered. Untracked files trigger a warning but don't block. Feature name is detected from CWD if omitted.

### Delete a project

```sh
pm delete                        # delete current project (with confirmation)
pm delete --project myapp        # delete a specific project by name
pm delete --force --yes          # skip safety checks and confirmation
```

Full project teardown: safety-checks all features, kills all tmux sessions, removes the `.pm/` directory, and removes the global registry entry. Blocks if any feature has uncommitted changes or unmerged commits unless `--force` is passed. Without `--force`, worktree directories and git branches are left on disk so you can continue using them as plain git repos. With `--force`, worktrees and branches are removed.

### Open a project

```sh
pm open                          # open/reconstruct sessions for current project
```

Creates tmux sessions for the main worktree and any active features that are missing sessions. Useful after a reboot or tmux server restart.

### Claude Code settings management

Manage Claude Code settings (`settings.json`, `settings.local.json`) across worktrees. The main worktree's `.claude/` directory is the source of truth — new features are seeded from it automatically.

```sh
pm claude settings list              # show .claude/ settings (works from main or feature worktree)
pm claude settings push              # push current feature's .claude/ settings to main
pm claude settings pull              # pull main's .claude/ settings into current feature
pm claude settings diff              # show differences between main and feature
pm claude settings merge             # union merge feature settings into main (main wins on conflicts)
pm claude settings merge --ours      # union merge, feature wins on conflicts
```

Feature name is detected from CWD if omitted.

### Claude Code session migration

When registering or adopting repos, Claude Code sessions are automatically migrated from the old path to the new worktree path. Sessions are copied (not moved) so originals remain accessible.

```sh
pm claude migrate --from /old/path   # migrate sessions from old path to CWD
pm feat adopt login --from /old/path          # adopt branch and migrate sessions
pm feat adopt login --context "..." --no-edit # adopt without auto-accept edits
pm feat adopt ciaran/eval --feature-name eval # adopt with custom feature name
```

`pm register` (both symlink and move modes) migrates sessions automatically — no extra flags needed.

### Project status dashboard

```sh
pm status                        # dashboard for current project
pm status --project myapp        # dashboard for a specific project by name
```

Shows a project overview: name, root path, feature count, and a table of all features with their status. For features with linked PRs, displays the PR number and GitHub state (open, merged, closed, draft). Also surfaces any health issues that `pm doctor` would flag.

### Diagnose project health

```sh
pm doctor                        # check all features in current project
pm doctor --project myapp        # check a specific project by name
pm doctor --fix                  # auto-fix clear-cut issues
```

Audits every feature for drift between pm state and external reality:

- Worktree directory exists on disk
- Git worktree registration matches
- Branch exists locally
- Tmux session exists (for active features)
- Status not stuck on "initializing"
- PR status matches (calls `gh` for features with linked PRs)

With `--fix`, auto-resolves clear-cut issues: removes orphaned state files (no worktree, no branch), cleans up stuck-initializing features, recreates missing tmux sessions, and updates status to match GitHub PR state. Ambiguous issues (e.g. missing directory but branch still exists) are skipped with a message.

### Bundled skills

pm ships with Claude Code skills that can be installed to a project or globally to `~/.claude/skills/`.

```sh
pm claude skills list                  # show available skills and install status
pm claude skills install               # install all bundled skills to project (main/.claude/skills/)
pm claude skills install pm            # install a specific skill to project
pm claude skills install --global      # install all bundled skills to ~/.claude/skills/
pm claude skills install pm --global   # install a specific skill globally
pm claude skills pull                  # pull main's skills into current feature
pm claude skills pull my-feat          # pull into a specific feature
```

Project-level skills are seeded to new features automatically. Use `pm claude skills pull` to sync skills added or updated on main after the feature was created. The `pm` skill teaches Claude Code agents how to dispatch features via `pm feat new` and `pm feat adopt`.

### Bundled agents

pm ships with Claude Code agent definitions that can be installed to a project or globally to `~/.claude/agents/`.

```sh
pm claude agents list                 # show available agents and install status
pm claude agents install              # install all bundled agents to project (main/.claude/agents/)
pm claude agents install implementer  # install a specific agent to project
pm claude agents install --global     # install all bundled agents to ~/.claude/agents/
pm claude agents uninstall reviewer   # uninstall a specific agent from the project
pm claude agents uninstall --all      # uninstall all bundled agents from the project
```

Bundled agents:

| Agent | Description |
|-------|-------------|
| **implementer** | Primary developer — reads TASK.md, implements changes, runs tests, addresses reviewer feedback |
| **reviewer** | Code reviewer — diffs the branch against base, evaluates quality/correctness, sends feedback |
| **researcher** | Read-only explorer — analyses the problem space, refines TASK.md before implementation begins |

### Agent management

```sh
pm agent spawn reviewer                      # spawn the reviewer agent in a new tmux window
pm agent spawn implementer --edit            # spawn with acceptEdits permission
pm agent spawn implementer --context "..."   # spawn with initial context/prompt
pm agent spawn                               # respawn all previously active agents

pm agent list                                # list all agents in the current feature
pm agent list --active                       # list only active agents
```

### Messaging

Agents within a feature communicate through a file-based message queue managed by pm. Each agent has an inbox scoped to the current feature.

Each inbox holds an ordered queue per sender, and a per-sender cursor
tracks "the last message I processed". Reading is a pure, single-message
operation. Advancing through the queue is an explicit `next` step.

```sh
pm msg send reviewer "ready for review"      # append to reviewer's inbox
pm msg send reviewer "msg" --as-agent impl   # send as a specific identity

pm msg wait                                  # block until any new message arrives
pm msg wait --from reviewer                  # block only on messages from reviewer

pm msg list                                  # enumerate inbox with cursor markers
pm msg list --from reviewer                  # only show one sender's queue

pm msg read                                  # print the next unread message
pm msg read --from reviewer                  # scope to one sender
pm msg read --from reviewer --index 3        # absolute: message 3 from reviewer
pm msg read --from reviewer --index +2       # peek ahead: cursor + 2
pm msg read --from reviewer --index -1       # re-read the last processed message (the cursor itself)
pm msg read --from reviewer --index -2       # one further back

pm msg next                                  # advance the cursor by one
pm msg next --from reviewer                  # scope to one sender
```

Key properties:

- **`read` never mutates.** Calling it ten times returns the same message.
- **`next` is the only mutation.** It advances the cursor by exactly one.
  Process a message, then call `next` to move on.
- **`--from` is required only when ambiguous.** If your inbox has unread
  messages from only one sender, `pm msg read` / `pm msg next` pick it
  automatically. If multiple senders have unread, you'll get a clear
  error asking you to disambiguate.
- **`--index` always requires `--from`.** Absolute and relative indices
  address a specific historical message — there's no "implicit sender"
  for that mode.
- **Re-reading is free.** Past messages stay on disk forever. Use
  `pm msg list` to find their index and `pm msg read --from <s> --index <n>`
  to dump them again.

Identity is resolved automatically: `PM_AGENT_NAME` (set by `pm agent spawn`) > `$USER` > `"user"`. Spawned agents get `PM_AGENT_NAME` set in their environment, so they don't need `--as-agent`.

#### Typical agent flow

1. `pm feat new my-feature --context "task description"` — creates the feature and spawns the default agent (usually `implementer`)
2. The implementer reads TASK.md, implements changes, runs tests
3. The implementer sends `pm msg send reviewer "ready for review"` to request a review
4. You (or the implementer) spawn the reviewer: `pm agent spawn reviewer`
5. The reviewer diffs the branch, sends feedback back to the implementer
6. The implementer addresses feedback, sends another "ready for review" message
7. The reviewer approves, and the implementer notifies you

#### Configuring the default agent

Set the default agent in `.pm/config.toml` to auto-spawn it on `pm feat new`:

```toml
[agents]
default = "implementer"

[agents.permissions]
implementer = "acceptEdits"
reviewer = ""
```

When `agents.default` is set, `pm feat new --context "..."` spawns that agent automatically. Override per-feature with `--agent`:

```sh
pm feat new my-feature --context "task" --agent researcher   # spawn researcher instead of default
pm feat new my-feature --context "task" --agent ""           # skip auto-spawn
```

The `agents.permissions` table controls the permission mode passed to `claude` for each agent. Set `"acceptEdits"` to auto-accept file edits, or leave empty for default permissions.

### Other commands

```sh
pm list                          # list all registered projects
pm open                          # open/reconstruct tmux sessions
pm --help                        # full help
pm feat --help                   # feature subcommand help
```

## Development

```sh
cargo build
cargo test
cargo clippy
cargo fmt
```

Tests spawn real tmux sessions. `cargo test` runs are capped at 4 threads via `.cargo/config.toml` to keep pty usage well under macOS limits. If you ever need to manually clean up stale test servers: `for s in /tmp/tmux-$(id -u)/pm-test-*; do tmux -L $(basename "$s") kill-server 2>/dev/null; rm -f "$s"; done`.

See `CLAUDE.md` for development guidelines.
