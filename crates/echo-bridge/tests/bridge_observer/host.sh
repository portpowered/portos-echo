#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
adapter="$root/crates/echo-bridge/tests/bridge_observer/host.py"
exec python3 "$adapter" "$@"
