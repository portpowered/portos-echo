#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
adapter="$root/crates/echo-bridge/tests/bridge_observer/host.py"
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    # shellcheck source=scripts/wsl-distro.sh
    . "$root/scripts/wsl-distro.sh"
    distro=$(echo_wsl_distro)
    adapter=$(echo_wsl_run "$distro" wslpath -u "$(cygpath -am "$adapter")")
    echo_wsl_relay "$root" "$distro" "$adapter" --observer -- "$@"
    ;;
esac
exec python3 "$adapter" "$@"
