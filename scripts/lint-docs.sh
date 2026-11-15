#!/bin/sh
set -eu

case "$#:$*" in
  0: | 1:--online) ;;
  *)
    printf '%s\n' 'usage: sh scripts/lint-docs.sh [--online]' >&2
    exit 2
    ;;
esac

# Provision separately; even the online link mode never fetches npm packages.
export npm_config_offline=true
if command -v python >/dev/null 2>&1 && python -c 'import sys; sys.exit(sys.version_info < (3, 12))' 2>/dev/null; then
  python_cmd=python
elif command -v python3 >/dev/null 2>&1 && python3 -c 'import sys; sys.exit(sys.version_info < (3, 12))' 2>/dev/null; then
  python_cmd=python3
else
  printf '%s\n' 'lint-docs: Python 3.12 is required' >&2
  exit 1
fi
exec "$python_cmd" scripts/lint_docs.py "$@"
