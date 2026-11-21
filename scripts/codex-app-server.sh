#!/bin/sh
# Print the path of the verified Linux x86_64 Codex app server for tests and the
# operator host. Online, this provisions the newest stable release (or the one
# CODEX_RELEASE names); with ECHO_CODEX_OFFLINE=1 it only reads what an earlier
# run provisioned. `--resolve` prints the selected release and fetches nothing.
# Python 3.12 is a repository prerequisite.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case "$#:${1:-}" in
  0:) exec python3 "$root/scripts/codex_release.py" host "$root" ;;
  1:--resolve) exec python3 "$root/scripts/codex_release.py" resolve "$root" ;;
esac
printf '%s\n' 'codex-app-server: takes no arguments, or --resolve' >&2
exit 1
