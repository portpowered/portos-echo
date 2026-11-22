#!/bin/sh
# Build and run the native Linux simulator, with the pinned fork as a rollback.
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
cache_root="$root/.cache/hassmic-host"
verify_source() {
  [ "$(git -C "$src" rev-parse HEAD)" = "$commit" ] || fail 'source HEAD differs from pin'
  [ -z "$(git -C "$src" status --porcelain --untracked-files=normal)" ] || fail 'source must be clean'
}
select_fork() {
  # Parse data, never source executable shell from the pin.
  repo=$(sed -n 's/^repo=//p' hassmic.pin)
  commit=$(sed -n 's/^commit=//p' hassmic.pin)
  [ "$repo" = 'https://github.com/portpowered/echo-dot-assist' ] || fail 'invalid pin repository'
  [ "${#commit}" -eq 40 ] || fail 'pin requires a full commit'
  case $commit in *[!0-9a-f]*) fail 'invalid pin commit' ;; esac
  [ "$(wc -l <hassmic.pin)" -eq 2 ] || fail 'pin requires exactly two lines'
  cache="$cache_root/$commit"
  if [ -n "${HASSMIC_SRC:-}" ]; then
    src=$(CDPATH='' cd -- "$HASSMIC_SRC" && pwd) || fail 'source directory missing'
    verify_source
  fi
}
build_fork() {
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
  built="$src/build/hassmic-host"
}
build_vendored() {
  target_dir=${CARGO_TARGET_DIR:-"$root/target"}
  cargo build --locked -p echo-bridge --bin echo-bridge --target-dir "$target_dir"
  built="$target_dir/debug/echo-bridge"
}
publish() {
  [ -x "$built" ] || fail 'build did not produce an executable'
  if [ -x "$binary" ] && cmp -s "$built" "$binary"; then
    printf 'hassmic-host: cached %s\n' "$binary"
  else
    cp "$built" "$lock/hassmic-host"
    mkdir -p "$cache"
    mv "$lock/hassmic-host" "$binary"
    printf 'hassmic-host: built %s\n' "$binary"
  fi
}
mode=${HASSMIC_HOST_FROM_FORK:-}
case $mode in
  '') cache="$cache_root/vendored" ;;
  1) select_fork ;;
  *) fail 'HASSMIC_HOST_FROM_FORK must be empty or 1' ;;
esac
binary="$cache/hassmic-host"
if [ "$command" = build ]; then
  if [ "$mode" = 1 ] && [ -x "$binary" ]; then
    printf 'hassmic-host: cached %s\n' "$binary"
    exit 0
  fi
  mkdir -p "$cache_root"
  lock="$cache.lock"
  mkdir "$lock" 2>/dev/null || fail 'another build owns this cache (or stale .lock directory)'
  trap 'rm -rf "$lock"' 0
  trap 'exit 1' HUP INT TERM
  if [ "$mode" = 1 ]; then
    build_fork
  else
    build_vendored
  fi
  publish
elif [ "$command" = path ]; then
  [ -x "$binary" ] || fail 'binary missing; run build first'
  printf '%s\n' "$binary"
else
  [ -x "$binary" ] || fail 'binary missing; run build first'
  exec "$binary" "$@"
fi
