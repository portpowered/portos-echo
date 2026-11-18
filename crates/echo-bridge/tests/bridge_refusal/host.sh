#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
script="$root/crates/echo-bridge/tests/bridge_refusal/host.py"
exec python3 "$script"
