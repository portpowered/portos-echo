#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
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
