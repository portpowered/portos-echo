#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    script=$(MSYS_NO_PATHCONV=1 wsl -d Ubuntu --exec wslpath -u "$(cygpath -am "$root/tests/hassmic_host_smoke.sh")")
    MSYS_NO_PATHCONV=1 exec wsl -d Ubuntu --exec sh -lc 'exec sh "$@"' sh "$script" "$@"
    ;;
esac
cd "$root"
python3 scripts/capture-wyoming.py --self-test
if [ "${1:-}" = --capture ]; then
  exec python3 scripts/capture-wyoming.py
fi
exec python3 scripts/capture-wyoming.py --smoke
