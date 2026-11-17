#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    # shellcheck source=scripts/wsl-distro.sh
    . "$root/scripts/wsl-distro.sh"
    distro=$(echo_wsl_distro)
    script=$(echo_wsl_run "$distro" wslpath -u "$(cygpath -am "$root/tests/hassmic_host_smoke.sh")")
    echo_wsl_exec "$distro" sh -lc 'exec sh "$@"' sh "$script" "$@"
    ;;
esac
cd "$root"
if [ "${1:-}" = --measure-barge-in ]; then
  exec python3 scripts/measure-barge-in.py
fi
python3 scripts/capture-wyoming.py --self-test
if [ "${1:-}" = --capture ]; then
  exec python3 scripts/capture-wyoming.py
fi
python3 scripts/capture-wyoming.py --smoke
python3 scripts/measure-barge-in.py --unit-test
exec python3 scripts/measure-barge-in.py --self-test
