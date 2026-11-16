#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
script="$root/crates/echo-bridge/tests/bridge_refusal/host.py"
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    script=$(MSYS_NO_PATHCONV=1 wsl -d Ubuntu --exec wslpath -u "$(cygpath -am "$script")")
    MSYS_NO_PATHCONV=1 exec wsl -d Ubuntu --exec python3 "$script"
    ;;
esac
exec python3 "$script"
