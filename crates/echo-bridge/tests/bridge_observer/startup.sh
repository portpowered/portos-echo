#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../../../.." && pwd)
script="$root/tests/test_host_startup.py"
PYTHONDONTWRITEBYTECODE=1 python3 "$script" -v
PYTHONDONTWRITEBYTECODE=1 exec python3 "$root/tests/test_host_teardown.py" -v
