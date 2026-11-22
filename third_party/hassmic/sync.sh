#!/bin/sh
# Refresh only the reviewed upstream inventory in this directory.
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
fail() {
  printf 'hassmic sync: %s\n' "$*" >&2
  exit 1
}
[ "$#" -eq 1 ] || fail 'usage: sync.sh <pinned-commit>'
commit=$1
pin=$(sed -n 's/^commit=//p' "$here/../../hassmic.pin")
[ "$commit" = "$pin" ] && [ "${#commit}" -eq 40 ] || fail 'commit must match hassmic.pin'
case $commit in *[!0-9a-f]*) fail 'expected a full lowercase Git commit' ;; esac
src=${HASSMIC_SRC:-"$here/../../.cache/hassmic-src/$commit"}
[ "$(git -C "$src" rev-parse HEAD)" = "$commit" ] || fail 'local source HEAD differs from pin'
[ -z "$(git -C "$src" status --porcelain --untracked-files=normal)" ] || fail 'local source must be clean'
version=$(TZ=UTC0 git -C "$src" log -1 --format=%cd --date=format-local:%Y.%m.%d.%H%M%S)
[ "$version" = "$(cat "$here/version")" ] || fail 'version does not match the pinned commit'

stage=$(mktemp -d "$here/.sync.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
trap 'exit 1' HUP INT TERM
while IFS=' ' read -r digest path stored_path; do
  mkdir -p -- "$stage/$(dirname -- "$path")"
  git -C "$src" cat-file blob "$commit:$path" >"$stage/$path"
  actual=$(sha256sum <"$stage/$path")
  [ "${actual%% *}" = "$digest" ] || fail "unreviewed source or licence for $path"
done <"$here/sources.sha256"
shfmt -w -i 2 -ci "$stage/tools/mkstubs.sh"

while IFS=' ' read -r digest path stored_path; do
  destination=${stored_path:-$path}
  mkdir -p -- "$here/$(dirname -- "$destination")"
  mv -- "$stage/$path" "$here/$destination"
done <"$here/sources.sha256"
