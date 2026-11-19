#!/bin/sh
# Manual Linux operator command. Hardware use belongs to the coordinator.
set -eu

help() {
  cat <<'HELP'
Usage: sh scripts/echo-install.sh --help
       sh scripts/echo-install.sh install --config FILE [--bridge FILE] [--codex FILE] [--ca FILE] [--force]
       sh scripts/echo-install.sh start
       sh scripts/echo-install.sh stop
       sh scripts/echo-install.sh status
       sh scripts/echo-install.sh uninstall [--purge]

Linux only; requires adb on PATH and exactly one ready device with hassmic.
--config FILE: required readable operator config; never printed or rewritten.
--bridge FILE: default ${CARGO_TARGET_DIR:-target}/armv7-linux-androideabi/release/echo-bridge
  (relative defaults are resolved from the checkout).
--codex FILE: default $CODEX_ARMV7_OUTPUT_DIR/codex-app-server; otherwise required.
--ca FILE: default $CODEX_ARMV7_OUTPUT_DIR/cacert.pem; otherwise required.
--force: replace bridge.toml only; existing prompts are always retained.
--purge: also remove private/ and home/ after a verified stop.

Device root: /data/local/echo-bridge
Artifacts: bin/echo-bridge, bin/codex-app-server, bin/cacert.pem
Config: private/bridge.toml (0600); private/ and home/ are 0700.
Config must select [codex] launch = "standalone",
binary = "/data/local/echo-bridge/bin/codex-app-server",
ca_file = "/data/local/echo-bridge/bin/cacert.pem".
W5 shared-home config: [home] dir = "/data/local/echo-bridge/home";
all secrets and codex.home stay in private/.
Supply config compatible with the bridge build; TOML is not validated or rewritten.
Prompt defaults: examples/prompts/*.md; seed missing home/prompts files only.
Absent defaults are skipped with a note.
Launch: setsid, /data/local/hassmic/runas root 3990,inet, held stdin; no boot integration.
start requires exact echo-bridge NAME from ps -A -o PID,NAME and a new
hassmic boot.log client connected line within 20 seconds.
stop closes held stdin and verifies absence; never kills an unknown process.
status shows process, uptime seconds, sanitized hassmic link and five bridge.log lines.
uninstall retains private/ and home/ unless --purge; never changes hassmic or /system.
Lifecycle commands will be delivered in the next implementation increment.
Exit status: 0 success/help/status observation; 1 operation/prerequisite failure;
2 usage error. Errors use fixed diagnostics without raw adb output.
Example: sh scripts/echo-install.sh install --config ./bridge.toml --bridge ./build/echo-bridge --codex ./build/codex-app-server --ca ./build/cacert.pem
HELP
}
fail() {
  printf 'echo-install: %s\n' "$1" >&2
  exit 1
}
usage() {
  printf '%s\n' 'echo-install: invalid usage; see --help' >&2
  exit 2
}

[ "$#" -gt 0 ] || usage
if [ "$1" = --help ]; then
  [ "$#" = 1 ] || usage
  help
  exit 0
fi
command_name=$1
shift
case $command_name in install | start | stop | status | uninstall) ;; *) usage ;; esac
config='' bridge='' codex='' ca='' force=false purge=false seen=' '
while [ "$#" -gt 0 ]; do
  flag=$1
  case $seen in *" $flag "*) usage ;; esac
  seen="$seen$flag "
  case $flag in
    --config | --bridge | --codex | --ca)
      [ "$command_name" = install ] && [ "$#" -ge 2 ] || usage
      case $2 in '' | --*) usage ;; esac
      case $flag in
        --config) config=$2 ;;
        --bridge) bridge=$2 ;;
        --codex) codex=$2 ;;
        --ca) ca=$2 ;;
      esac
      shift
      ;;
    --force)
      [ "$command_name" = install ] || usage
      force=true
      ;;
    --purge)
      [ "$command_name" = uninstall ] || usage
      purge=true
      ;;
    *) usage ;;
  esac
  shift
done
# Reserved until the lifecycle increment; never partially execute these commands.
[ "$command_name" = install ] || fail 'lifecycle command not yet available'
[ "$purge" = false ] || usage
[ -n "$config" ] || usage
checkout=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
target=${CARGO_TARGET_DIR:-target}
case $target in /*) ;; *) target=$checkout/$target ;; esac
bridge=${bridge:-$target/armv7-linux-androideabi/release/echo-bridge}
if [ -n "${CODEX_ARMV7_OUTPUT_DIR:-}" ]; then
  codex=${codex:-$CODEX_ARMV7_OUTPUT_DIR/codex-app-server}
  ca=${ca:-$CODEX_ARMV7_OUTPUT_DIR/cacert.pem}
fi
for input in "$config" "$bridge" "$codex" "$ca"; do
  [ -f "$input" ] && [ -r "$input" ] || fail 'required local input is not readable'
done
command -v adb >/dev/null 2>&1 || fail 'adb is required'
umask 077
log=$(mktemp) || fail 'private log creation failed'
locked=false
safe_paths=false
device=/data/local/echo-bridge
lock=/data/local/echo-bridge.lock
cleanup() {
  if [ "$locked" = true ]; then
    if [ "$safe_paths" = true ]; then
      adb shell "rm -f '$device/private/.install-stage'" >/dev/null 2>&1 || :
    fi
    adb shell "rmdir '$lock'" >/dev/null 2>&1 || :
  fi
  rm -f "$log"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
run_adb() {
  if [ "$1" = shell ]; then
    adb shell "set -e
$2" >"$log" 2>/dev/null || fail 'device operation failed'
  else
    adb "$@" >"$log" 2>/dev/null || fail 'device operation failed'
  fi
}
run_adb devices
selection=$(awk 'NR > 1 && NF { count++; if ($2 == "device") ready++ }
  END { print (count == 1 && ready == 1) ? "ready" : "refuse" }' "$log")
[ "$selection" = ready ] || fail 'exactly one ready adb device is required'
run_adb shell "test -x /data/local/hassmic/runas"
run_adb shell "umask 077; mkdir '$lock'"
locked=true
run_adb shell "ps -A -o PID,NAME"
running=$(awk '$2 == "echo-bridge" { found=1 } END { print found+0 }' "$log")
[ "$running" = 0 ] || fail 'bridge is already running; stop before install'
# Fixed destinations only. Refuse links before chmod, redirects, push or rename.
run_adb shell "for p in '$device' '$device/bin' '$device/private' '$device/home' '$device/home/prompts'; do
  test ! -L \"\$p\" && { test ! -e \"\$p\" || test -d \"\$p\"; } || exit 1
done
umask 077
mkdir -p '$device/bin' '$device/private' '$device/home/prompts' &&
chmod 755 '$device' '$device/bin' &&
chmod 700 '$device/private' '$device/home' '$device/home/prompts'"
safe_paths=true

# Each copy is staged privately; failed transfers never replace an existing file.
# The destination leaf is fixed or a restricted prompt basename, never shell input.
copy_file() {
  local_file=$1 destination=$2 mode=$3 replace=$4
  run_adb shell "test ! -L '$destination' && { test ! -e '$destination' || test -f '$destination'; }"
  if [ "$replace" = false ]; then
    run_adb shell "if test -f '$destination'; then printf retained; else printf missing; fi"
    if [ "$(awk '{print}' "$log")" = retained ]; then
      run_adb shell "chmod '$mode' '$destination'"
      return 0
    fi
  fi
  run_adb shell "test ! -e '$device/private/.install-stage' && test ! -L '$device/private/.install-stage'"
  run_adb push "$local_file" "$device/private/.install-stage"
  run_adb shell "chmod '$mode' '$device/private/.install-stage' && mv -f '$device/private/.install-stage' '$destination'"
}
copy_file "$bridge" "$device/bin/echo-bridge" 755 true
copy_file "$codex" "$device/bin/codex-app-server" 755 true
copy_file "$ca" "$device/bin/cacert.pem" 644 true
copy_file "$config" "$device/private/bridge.toml" 600 "$force"
prompts=false
for prompt in "$checkout"/examples/prompts/*.md; do
  [ -f "$prompt" ] || continue
  name=${prompt##*/}
  case $name in *[!a-zA-Z0-9_.-]*) fail 'unsupported prompt filename' ;; esac
  copy_file "$prompt" "$device/home/prompts/$name" 600 false
  prompts=true
done
run_adb shell "test ! -L '$device/private/launch.sh'
test ! -L '$device/private/bridge.log'
umask 077
cat > '$device/private/.install-stage' <<'LAUNCH'
#!/system/bin/sh
umask 077
exec /data/local/hassmic/runas root 3990,inet /data/local/echo-bridge/bin/echo-bridge --config /data/local/echo-bridge/private/bridge.toml --trace /data/local/echo-bridge/private/bridge.trace
LAUNCH
chmod 700 '$device/private/.install-stage' &&
mv -f '$device/private/.install-stage' '$device/private/launch.sh' &&
touch '$device/private/bridge.log' && chmod 600 '$device/private/bridge.log'"
[ "$prompts" = true ] || printf '%s\n' 'echo-install: prompt defaults absent; skipped'
printf '%s\n' 'echo-install: installed'
