#!/bin/sh
# T0 regression: exercise the delivered gate in an isolated Git repository.
set -eu

runner=$(pwd)/scripts/lint-scripts-web.sh
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' 0
trap 'exit 1' HUP INT TERM
cd "$fixture"
git init -q
mkdir -p factory scripts
printf "#!/bin/sh\nprintf '%%s\\\\n' \$1\n" >factory/violation.sh
cp factory/violation.sh 'scripts/space é.sh'
git add factory/violation.sh

# An excluded tracked violation and an untracked violation must have no effect.
sh "$runner" >actual 2>&1
printf '%s\n' \
  'lint-scripts-web: excluded factory/' \
  'lint-scripts-web: 0 shell files' \
  'web/screen: no package.json yet, skipped' >expected
diff -u expected actual

# The identical content becomes a failure when tracked outside factory/.
git add 'scripts/space é.sh'
if sh "$runner" >actual 2>&1; then
  printf '%s\n' 'FAIL: tracked SC2086 violation was accepted' >&2
  exit 1
fi
grep -F SC2086 actual
# Windows shellcheck may render the Unicode character using the console codepage.
grep -F 'In scripts/space ' actual
printf "#!/bin/sh\nprintf '%%s\\\\n' \"\$1\"\n" >'scripts/space é.sh'
sh "$runner" >actual 2>&1
grep -Fx 'lint-scripts-web: 1 shell files' actual
grep -Fx 'web/screen: no package.json yet, skipped' actual
printf '%s\n' 'PASS: factory excluded; tracked spaced Unicode path checked; untracked source ignored'
