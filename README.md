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
```

Creates a git branch, worktree, and tmux session (`myapp/login`).

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
pm feat merge login --delete    # merge and clean up (session, worktree, branch, state)
```

Blocks if either the feature or main worktree has uncommitted changes. Always creates a merge commit (`--no-ff`).

### Delete a feature

```sh
pm feat delete login             # with safety checks
pm feat delete login --force     # skip safety checks
```

Safety checks block deletion if the feature has uncommitted changes or commits not merged into main. Untracked files trigger a warning but don't block.

### Other commands

```sh
pm list                          # list all registered projects
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
