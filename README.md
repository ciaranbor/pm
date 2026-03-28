# pm

Terminal-based project manager built around tmux and git worktrees.

## Requirements

- tmux
- git

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
```

Creates a git branch, worktree, and tmux session (`myapp/login`). With `--context`, seeds a `TASK.md` in the worktree.

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
pm feat merge login             # merge into main
pm feat merge --delete          # merge current feature and clean up
```

Blocks if either the feature or main worktree has uncommitted changes. Always creates a merge commit (`--no-ff`). Feature name is detected from CWD if omitted.

### Lifecycle hooks

Every project is bootstrapped with two hook scripts:

- `.pm/hooks/post-create.sh` — runs in the feature's tmux session after `pm feat new`
- `.pm/hooks/post-merge.sh` — runs in the main tmux session after `pm feat merge`

Hooks run asynchronously in a dedicated `hook` window via `tmux send-keys` — pm does not block on their completion. The hook window is reused across invocations.

Edit the scripts to add your own logic (install deps, run migrations, deploy, etc.). Removing a hook script disables it.

### Delete a feature

```sh
pm feat delete login             # with safety checks
pm feat delete --force           # delete current feature, skip safety checks
```

Safety checks block deletion if the feature has uncommitted changes or commits not merged into main. Untracked files trigger a warning but don't block. Feature name is detected from CWD if omitted.

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
