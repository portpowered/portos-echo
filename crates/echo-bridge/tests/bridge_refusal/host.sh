#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
script="$root/crates/echo-bridge/tests/bridge_refusal/host.py"
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    # shellcheck source=scripts/wsl-distro.sh
    . "$root/scripts/wsl-distro.sh"
    distro=$(echo_wsl_distro)
    script=$(echo_wsl_run "$distro" wslpath -u "$(cygpath -am "$script")")
    echo_wsl_relay "$root" "$distro" "$script" --
    ;;
esac
exec python3 "$script"
