#!/bin/sh
# Build only from checked-in poses with local Linux media tools.
set -eu
exec python3 -B "$(dirname "$0")/packs/companion/build.py" "$@"
