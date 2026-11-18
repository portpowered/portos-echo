#!/bin/sh
# Build and run the pinned native Linux host.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
fail() {
  printf 'hassmic-host: %s\n' "$*" >&2
  exit 1
}
command=${1:-}
[ "$#" -gt 0 ] && shift
case $command in
  build | path) [ "$#" -eq 0 ] || fail 'build/path take no arguments' ;;
  run) ;;
  *) fail 'usage: hassmic-host.sh build|path|run [args...]' ;;
esac
# Parse data, never source executable shell from the pin.
repo=$(sed -n 's/^repo=//p' hassmic.pin)
commit=$(sed -n 's/^commit=//p' hassmic.pin)
[ "$repo" = 'https://github.com/portpowered/echo-dot-assist' ] || fail 'invalid pin repository'
[ "${#commit}" -eq 40 ] || fail 'pin requires a full commit'
case $commit in *[!0-9a-f]*) fail 'invalid pin commit' ;; esac
[ "$(wc -l <hassmic.pin)" -eq 2 ] || fail 'pin requires exactly two lines'
cache="$root/.cache/hassmic-host/$commit"
binary="$cache/hassmic-host"
verify_source() {
  [ "$(git -C "$src" rev-parse HEAD)" = "$commit" ] || fail 'source HEAD differs from pin'
  [ -z "$(git -C "$src" status --porcelain --untracked-files=normal)" ] || fail 'source must be clean'
}
if [ -n "${HASSMIC_SRC:-}" ]; then
  src=$(CDPATH='' cd -- "$HASSMIC_SRC" && pwd) || fail 'source directory missing'
  verify_source
fi
if [ "$command" = build ]; then
  if [ -x "$binary" ]; then
    printf 'hassmic-host: cached %s\n' "$binary"
    exit 0
  fi
  mkdir -p "$root/.cache/hassmic-host"
  lock="$cache.lock"
  mkdir "$lock" 2>/dev/null || fail 'another build owns this pin (or stale .lock directory)'
  trap 'rm -rf "$lock"' 0
  trap 'exit 1' HUP INT TERM
  if [ -z "${HASSMIC_SRC:-}" ]; then
    src="$root/.cache/hassmic-src/$commit"
    if [ ! -d "$src" ]; then
      git init -q "$lock/source"
      git -C "$lock/source" fetch --depth 1 "$repo" "$commit"
      # Materialize build inputs only, without upstream test-only fake secrets.
      git -C "$lock/source" sparse-checkout set src devices tools web keys
      git -C "$lock/source" checkout -q --detach FETCH_HEAD
      mkdir -p "$root/.cache/hassmic-src"
      mv "$lock/source" "$src"
    fi
    verify_source
  fi
  # Force compilation even if a supplied checkout has ignored build artifacts.
  make -C "$src" -B build/hassmic-host DEVICE=athena
  [ -x "$src/build/hassmic-host" ] || fail 'build did not produce an executable'
  cp "$src/build/hassmic-host" "$lock/hassmic-host"
  mkdir -p "$cache"
  mv "$lock/hassmic-host" "$binary"
  printf 'hassmic-host: built %s\n' "$binary"
elif [ "$command" = path ]; then
  [ -x "$binary" ] || fail 'binary missing; run build first'
  printf '%s\n' "$binary"
else
  [ -x "$binary" ] || fail 'binary missing; run build first'
  exec "$binary" "$@"
fi
