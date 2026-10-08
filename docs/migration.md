# Moving to another machine

`.pm/` holds a project's state (features, agents, messages, config,
summaries, docs); the pm config dir holds the project registry, global
config, notices, and your global workflows. Both can be git-backed. pm's
state, cache and runtime dirs are this machine's alone and never move
([Where pm keeps its files](../README.md#where-pm-keeps-its-files)). Your
code travels through each repo's own remote, and agents' conversations
through a `pm harness export` tarball.

`pm migrate check` says what a move would lose or fail on — unpushed
branches and state, uncommitted work, agents still running, and the
machine-local things to redo by hand (harness logins, `pm serve` devices,
your tmux config, your own global skills) — with the plan that clears it.
A feature it marks in flight (uncommitted work, or commits not merged into
its base) is better finished and merged before the move than committed
half-done.

On the old machine, once, each against a new, empty repo:

```sh
pm state init --global --remote <registry-url>   # the registry
pm state init --remote <state-url>               # in each project (or `pm state remote` for an existing .pm/ repo)
pm state backfill                                # record repo and state URLs in the registry
```

Then, at each move, from a shell outside pm's tmux sessions:

```sh
pm close --all                  # stop agents; they stay active, so the new machine resumes them
pm state push                   # in each project, then:
pm state push --global
pm migrate check --project <name>…   # until it passes
pm harness export --all --project <name>… --harness <h> -o pm-<h>.tar.gz   # once per harness your agents use
```

On the new machine, with pm ([Install](../README.md#install)), git with
access to your remotes, `gh`, tmux and your harnesses installed:

```sh
pm state init --global --remote <registry-url>
pm restore --project <name>… --import pm-claude-code.tar.gz   # clone, pull state, recreate worktrees, import, then start agents
```

`pm state init --global --remote` is safe to repeat: one that can't fetch
leaves nothing behind, and with the registry already on that remote it
pulls again. Where it can't fast-forward — this machine registered
projects too — it takes the remote's registry and keeps the projects only
this machine has, setting aside any the remote also has in the config
dir's `registry-before-pull/`. `pm restore` without `--project` restores
every registered project. It starts agents only after the `--import`
tarballs are in, so each resumes its conversation, and lists last any
worktree whose sessions it could not import; `pm harness import <tarball>`
imports one later, into projects already restored. An import adds only
what the machine lacks, never replacing a session or memory file it has,
and rewrites the recorded paths when the home directory differs.
`pm harness migrate --from <old path>` does the same for a project moved
on one machine. A root that exists without its `main/` checkout is completed
too: restore clones into `main/`, writing a fresh project config when the
root has none and the state remote doesn't supply one.

The registry repo syncs your global custom workflows, never the bundled
ones or machine-local files.
