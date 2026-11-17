#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    # shellcheck source=scripts/wsl-distro.sh
    . "$root/scripts/wsl-distro.sh"
    distro=$(echo_wsl_distro)
    script=$(echo_wsl_run "$distro" wslpath -u "$(cygpath -am "$root/tests/wyoming_host.py")")
    echo_wsl_exec "$distro" python3 "$script" "$@"
    ;;
esac
exec python3 "$root/tests/wyoming_host.py" "$@"
