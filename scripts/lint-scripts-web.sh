#!/bin/sh
set -eu

# Shared Phase F exclusion: vendored factory code has its own gates.
FACTORY_EXCLUDE='factory/'
readonly FACTORY_EXCLUDE
printf 'lint-scripts-web: excluded %s\n' "$FACTORY_EXCLUDE"

require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'lint-scripts-web: required tool missing: %s\n' "$1" >&2
    exit 1
  fi
}

for tool in git shellcheck shfmt mktemp rm xargs tr wc; do
  require_tool "$tool"
done

manifest=$(mktemp)
trap 'rm -f "$manifest"' 0
trap 'exit 1' HUP INT TERM
if ! git ls-files -z -- '*.sh' ":(exclude)$FACTORY_EXCLUDE" >"$manifest"; then
  printf '%s\n' 'lint-scripts-web: git ls-files failed' >&2
  exit 1
fi
count=$(tr -cd '\000' <"$manifest" | wc -c)
printf 'lint-scripts-web: %s shell files\n' "$count"
if [ -s "$manifest" ]; then
  xargs -0 shellcheck --severity=style -- <"$manifest"
  xargs -0 shfmt -d -i 2 -ci -- <"$manifest"
fi

if [ ! -f web/screen/package.json ]; then
  printf '%s\n' 'web/screen: no package.json yet, skipped'
  exit 0
fi

require_tool npm
require_tool npx
cd web/screen
npm ci
for tool in eslint tsc prettier; do
  if [ ! -x "node_modules/.bin/$tool" ]; then
    printf 'lint-scripts-web: required local tool missing: %s\n' "$tool" >&2
    exit 1
  fi
done
# All tools must be installed locally; npx must never fetch a replacement.
export npm_config_offline=true npm_config_yes=false
npm run check:schema
npx eslint .
npx tsc --noEmit
npx prettier --check .
