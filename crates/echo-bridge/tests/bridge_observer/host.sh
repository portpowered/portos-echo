#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
adapter="$root/crates/echo-bridge/tests/bridge_observer/host.py"
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    adapter=$(MSYS_NO_PATHCONV=1 wsl -d Ubuntu --exec wslpath -u "$(cygpath -am "$adapter")")
    MSYS_NO_PATHCONV=1 exec wsl -d Ubuntu --exec python3 "$adapter" "$@"
    ;;
esac
exec python3 "$adapter" "$@"
