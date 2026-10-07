#!/bin/sh
# Stand-in for `claude`/`codex`/`opencode` inside a pm sandbox: answers pm's
# capability probes and pre-launch calls, otherwise records how it was
# invoked, starts its session (pm's SessionStart hook), and holds the window
# open like an agent between turns: pm's Stop hook is its waiter, so it reads
# idle until a message arrives, then busy. One record per invocation (pid
# suffix) so a respawn leaves a second one, renamed into place once complete
# so a reader never sees it half written.
name=$(basename "$0")
case $1 in
  --help) echo "  --append-system-prompt-file <file>"; exit 0 ;;
  --version)
    case $name in
      opencode) echo "opencode v2.0.23" ;;
      *) echo "$name 0.156.0" ;;
    esac
    exit 0 ;;
esac
if [ "$name" = opencode ]; then
  # Without it opencode starts a server shared by every agent of this HOME.
  case " $* " in
    *" --standalone "*) ;;
    *) echo "opencode shim: --standalone missing: $*" >&2; exit 2 ;;
  esac
  if [ "$1" = api ]; then
    {
      printf 'argc=%s\n' "$#"
      printf '%s\n' "$0" "$@"
      printf 'cwd=%s\nPM_AGENT_NAME=%s\n' "$PWD" "$PM_AGENT_NAME"
      printf 'OPENCODE_CONFIG=%s\nOPENCODE_CONFIG_CONTENT=%s\n' \
        "$OPENCODE_CONFIG" "$OPENCODE_CONFIG_CONTENT"
    } > "$HOME/log/$name-api-$$.tmp"
    mv "$HOME/log/$name-api-$$.tmp" "$HOME/log/$name-api-$$.api"
    echo "{\"data\":{\"id\":\"ses_shim$$\"}}"
    exit 0
  fi
fi
{
  printf 'argc=%s\n' "$#"
  printf '%s\n' "$0" "$@"
  printf 'cwd=%s\nPM_AGENT_NAME=%s\n' "$PWD" "$PM_AGENT_NAME"
  if [ "$name" = claude ]; then
    # Whether the session `--resume` names is in the store, under the key of
    # the resolved cwd, as Claude Code looks it up.
    key=$(pwd -P | sed 's/[^A-Za-z0-9]/-/g')
    prev=""
    for a in "$@"; do
      if [ "$prev" = --resume ]; then
        if [ -f "$HOME/.claude/projects/$key/$a.jsonl" ]; then
          echo "resumed=found"
        else
          echo "resumed=missing"
        fi
      fi
      prev=$a
    done
  fi
  if [ "$name" = opencode ]; then
    printf 'PM_OPENCODE_SESSION=%s\n' "$PM_OPENCODE_SESSION"
    printf 'OPENCODE_CONFIG=%s\nOPENCODE_CONFIG_CONTENT=%s\n' \
      "$OPENCODE_CONFIG" "$OPENCODE_CONFIG_CONTENT"
  fi
} > "$HOME/log/$name-$$.tmp"
mv "$HOME/log/$name-$$.tmp" "$HOME/log/$name-${PM_AGENT_NAME:-default}-$$.argv"
# The hook catches Ctrl-C and exits cleanly, which alone would not end `sh`.
trap 'exit 130' INT
# While $HOME/shim-hang exists it is held before its session starts, drawing
# nothing, as a harness is on a keychain that does not answer.
while [ -e "$HOME/shim-hang" ]; do sleep 1; done
# The session it starts: the one it was told to open or resume, else new.
session=${PM_OPENCODE_SESSION:-shim-$$}
prev=""
for a in "$@"; do
  case $prev in --resume|resume) session=$a ;; esac
  prev=$a
done
echo "{\"session_id\":\"$session\"}" | pm harness hooks session-start >/dev/null 2>&1
pm harness hooks stop </dev/null >/dev/null 2>&1
# While $HOME/shim-turns exists, each wake is a turn that reads the inbox and
# ends, so the agent goes idle again.
while [ -e "$HOME/shim-turns" ]; do
  pm msg read >/dev/null 2>&1
  pm harness hooks stop </dev/null >/dev/null 2>&1
done
exec sleep 600
