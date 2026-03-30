# pm

Terminal-based project manager built around tmux and git worktrees.

## Requirements

- tmux
- git
- [gh](https://cli.github.com/) (for `pm feat pr`, `pm feat sync`)

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
```

Creates a git branch, worktree, and tmux session (`myapp/login`). With `--context`, seeds a `TASK.md` in the worktree. With `--base`, branches from the specified branch instead of the default. When `--base` is omitted, the current branch is detected from CWD — so running `pm feat new child` from within a feature worktree automatically stacks on that feature.

### List features

```sh
pm feat list
```

### Switch to a feature

```sh
pm feat switch login             # direct switch
pm feat switch                   # interactive picker (tmux display-menu)
```

### Merge a feature

```sh
pm feat merge login             # merge into base branch (defaults to main)
pm feat merge --delete          # merge current feature and clean up
```

Merges the feature into its base branch (defaults to main; stacked features merge into their parent). Blocks if either the feature or base worktree has uncommitted changes. Always creates a merge commit (`--no-ff`). Feature name is detected from CWD if omitted. If the branch was already merged (e.g. via GitHub PR), the local merge is skipped. If a merge conflict occurs, the merge is aborted and the base worktree is left clean.

### Lifecycle hooks

Every project is bootstrapped with two hook scripts:

- `.pm/hooks/post-create.sh` — runs in the feature's tmux session after `pm feat new`
- `.pm/hooks/post-merge.sh` — runs in the base branch's tmux session after `pm feat merge`

Hooks run asynchronously in a dedicated `hook` window via `tmux send-keys` — pm does not block on their completion. The hook window is reused across invocations.

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

### Open a project

```sh
pm open                          # open/reconstruct sessions for current project
```

Creates tmux sessions for the main worktree and any active features that are missing sessions. Useful after a reboot or tmux server restart.

### Permissions management

Manage Claude Code settings (`settings.json`, `settings.local.json`) across worktrees. The main worktree's `.claude/` directory is the source of truth — new features are seeded from it automatically.

```sh
pm perm list              # show current feature's .claude/ settings
pm perm push              # push current feature's .claude/ settings to main
pm perm pull              # pull main's .claude/ settings into current feature
pm perm diff              # show differences between main and feature
pm perm merge             # union merge feature settings into main (main wins on conflicts)
pm perm merge --ours      # union merge, feature wins on conflicts
```

Feature name is detected from CWD if omitted.

### Claude Code session migration

When registering or adopting repos, Claude Code sessions are automatically migrated from the old path to the new worktree path. Sessions are copied (not moved) so originals remain accessible.

```sh
pm claude migrate --from /old/path   # migrate sessions from old path to CWD
pm feat adopt login --from /old/path # adopt branch and migrate sessions
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

See `design.md` for the full spec and `CLAUDE.md` for development guidelines.
