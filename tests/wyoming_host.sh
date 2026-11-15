#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    script=$(MSYS_NO_PATHCONV=1 wsl -d Ubuntu --exec wslpath -u "$(cygpath -am "$root/tests/wyoming_host.py")")
    MSYS_NO_PATHCONV=1 exec wsl -d Ubuntu --exec python3 "$script" "$@"
    ;;
esac
exec python3 "$root/tests/wyoming_host.py" "$@"
