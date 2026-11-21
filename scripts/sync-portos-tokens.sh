#!/bin/sh
# Vendors the generated Port OS design tokens crate and the Manrope font the tokens name.
#   sync-portos-tokens.sh --check                  verify the vendored files against the manifest (T0, no network)
#   sync-portos-tokens.sh <rev> [<font-checkout>]  fetch the crate at <rev> and rewrite the manifest
# <rev> is a tag, a branch or a full commit of the public crate repository; it is fetched into a temporary
# directory outside this tree. With <font-checkout>, a Port OS monorepo checkout with its packages installed,
# the font and its licence are copied again from @fontsource-variable/manrope; without it they stay as they are.
# Never edit the vendored files by hand.
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
repository=https://github.com/portpowered/portos-ui-rust
vendor=third_party/portos-tokens
fonts=web/screen/src/fonts
manifest=$vendor/vendor.sha256
package=frontend/packages/ui/node_modules/@fontsource-variable/manrope
font_version=5.3.0
crate_files='Cargo.toml LICENSE README.md src/lib.rs'
cd "$root"

fail() {
  printf 'sync-portos-tokens: %s\n' "$1" >&2
  exit 1
}

vendored() {
  for file in $crate_files; do
    printf '%s\n' "$vendor/$file"
  done
  printf '%s\n' "$fonts/manrope-latin-wght-normal.woff2" "$fonts/OFL.txt"
}

digest() {
  [ -f "$1" ] || fail "missing vendored file $1"
  sum=$(sha256sum <"$1")
  printf '%s  %s\n' "${sum%% *}" "$1"
}

check() {
  [ -f "$manifest" ] || fail "missing $manifest"
  vendored | while read -r file; do
    expected=$(grep -F "  $file" "$manifest" || true)
    actual=$(digest "$file")
    if [ "$expected" != "$actual" ]; then
      fail "$file differs from $manifest; rerun the sync, never edit vendored files"
    fi
  done
  count=$(grep -c -v '^#' "$manifest")
  [ "$count" -eq 6 ] || fail "$manifest lists $count files, expected 6"
  printf 'sync-portos-tokens: 6 vendored files match %s\n' "$manifest"
}

copy_font() {
  grep -q "\"version\": \"$font_version\"" "$1/$package/package.json" ||
    fail "expected @fontsource-variable/manrope $font_version in $1/$package"
  mkdir -p "$fonts"
  cp "$1/$package/files/manrope-latin-wght-normal.woff2" "$fonts/manrope-latin-wght-normal.woff2"
  cp "$1/$package/LICENSE" "$fonts/OFL.txt"
}

sync() {
  rev=$1
  work=$(mktemp -d)
  trap 'rm -rf "$work"' 0
  trap 'exit 1' HUP INT TERM
  git init -q "$work"
  git -C "$work" fetch -q --depth 1 "$repository" "$rev" || fail "cannot fetch $rev from $repository"
  commit=$(git -C "$work" rev-parse --verify 'FETCH_HEAD^{commit}')
  mkdir -p "$vendor/src"
  for file in $crate_files; do
    git -C "$work" show "$commit:$file" >"$vendor/$file"
  done
  if [ -n "${2:-}" ]; then
    copy_font "$2"
  fi
  {
    printf '# Written by scripts/sync-portos-tokens.sh. Do not edit by hand.\n'
    printf '# repository: %s\n' "$repository"
    printf '# commit: %s\n' "$commit"
    printf '# font: @fontsource-variable/manrope %s (OFL-1.1)\n' "$font_version"
    vendored | while read -r file; do
      digest "$file"
    done
  } >"$manifest"
  check
}

case "${1:-}" in
  --check) check ;;
  '') fail 'usage: sync-portos-tokens.sh --check | <rev> [<font-checkout>]' ;;
  *) sync "$@" ;;
esac
