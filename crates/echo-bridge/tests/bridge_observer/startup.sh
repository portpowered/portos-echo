#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
script="$root/tests/test_host_startup.py"
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    # shellcheck source=scripts/wsl-distro.sh
    . "$root/scripts/wsl-distro.sh"
    distro=$(echo_wsl_distro)
    adapter=$(echo_wsl_run "$distro" wslpath -u "$(cygpath -am "$0")")
    echo_wsl_exec "$distro" sh "$adapter"
    ;;
esac
PYTHONDONTWRITEBYTECODE=1 python3 "$script" -v
PYTHONDONTWRITEBYTECODE=1 exec python3 "$root/tests/test_host_teardown.py" -v
