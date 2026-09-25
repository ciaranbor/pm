#!/bin/sh
# Stand-in for `claude`/`codex` inside a pm sandbox: answers pm's capability
# probes, otherwise records how it was invoked and holds the window open like
# a running agent would. One record per invocation (pid suffix) so a respawn
# leaves a second one.
case $1 in
  --help) echo "  --append-system-prompt-file <file>"; exit 0 ;;
  --version) echo "$(basename "$0") 0.153.2"; exit 0 ;;
esac
{
  printf 'argc=%s\n' "$#"
  printf '%s\n' "$0" "$@"
  printf 'cwd=%s\nPM_AGENT_NAME=%s\n' "$PWD" "$PM_AGENT_NAME"
} > "$HOME/log/$(basename "$0")-${PM_AGENT_NAME:-default}-$$.argv"
exec sleep 600
