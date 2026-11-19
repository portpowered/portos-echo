#!/bin/sh
# Manual Linux operator command. Hardware use belongs to the coordinator.
set -eu

# Fixed product identity, never a caller override or evaluated shell fragment.
ECHO_RUN_AS='nobody 3990,inet'
ECHO_RUN_OWNER='9999:9999'
readonly ECHO_RUN_AS ECHO_RUN_OWNER
run_user=${ECHO_RUN_AS%% *}
run_groups=${ECHO_RUN_AS#* }

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
Launch: setsid, /data/local/hassmic/runas nobody 3990,inet, held stdin; no boot integration.
start requires exact echo-bridge NAME from ps -A -o PID,NAME and a new
hassmic boot.log client connected line within 20 seconds.
stop closes held stdin and verifies absence; never kills an unknown process.
private/ and home/: owner 9999:9999, directories 0700, data files 0600.
bin/: owner 0:0, directory and binaries 0755; cacert.pem 0644.
install and start migrate retained root-owned state.
status shows process, process user, uptime seconds, sanitized hassmic link and five bridge.log lines.
Status fields: process=running|stopped; process_user=USER|unavailable;
uptime_seconds=INTEGER|unavailable;
hassmic_link=SANITIZED_LINE|unavailable
uninstall retains private/ and home/ unless --purge; never changes hassmic or /system.
Success: echo-install: uninstalled (state retained)
Success with --purge: echo-install: uninstalled (purged)
A running root process fails; unverifiable running identity fails closed.
Exit status: 0 success/help/safe status observation; 1 operation/prerequisite/unsafe process-user failure;
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
if [ "$command_name" = install ]; then
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
fi
command -v adb >/dev/null 2>&1 || fail 'adb is required'
umask 077
log=$(mktemp) || fail 'private log creation failed'
locked=false
safe_paths=false
starting=false
device=/data/local/echo-bridge
lock=/data/local/echo-bridge.lock
# Claim the unconsumed launch gate, or ask the owning session to close stdin.
# A late launcher cannot pass a gate removed by cancellation.
close_stdin="
if test -d '$device/private/run'; then
  if rmdir '$device/private/run/control' 2>/dev/null; then
    touch '$device/private/run/finished'
  else
    touch '$device/private/run/stop'
  fi
  end=\$((\$(date +%s) + 20))
  while ! test -f '$device/private/run/finished'; do
    test \"\$(date +%s)\" -lt \"\$end\" || exit 1
    sleep 1
  done
fi"
cleanup() {
  if [ "$starting" = true ]; then
    adb shell "set -e
$close_stdin
observed=\$(ps -A -o PID,NAME)
printf '%s\\n' \"\$observed\" | awk '\$2 == \"echo-bridge\" { found=1 } END { exit found+0 }'
rm -rf '$device/private/run'" >/dev/null 2>&1 || :
  fi
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
process_state() {
  run_adb shell "ps -A -o PID,NAME"
  running=$(awk '$2 == "echo-bridge" { found=1 } END { print found+0 }' "$log")
}
process_state
# All commands refuse redirected state before any read, write or chmod.
run_adb shell "for p in '$device' '$device/bin' '$device/private' '$device/home' '$device/home/prompts' '$device/private/run'; do
  test ! -L \"\$p\" && { test ! -e \"\$p\" || test -d \"\$p\"; } || exit 1
done
for p in launch.sh bridge.log bridge.toml bridge.trace; do test ! -L '$device/private/'\$p || exit 1; done
if test -d '$device/private/run'; then
  for p in stdin control stop finished deadline log-inode log-prefix; do test ! -L '$device/private/run/'\$p || exit 1; done
fi"
if [ "$command_name" = status ]; then
  # Read-only observation. Optional unsupported/missing proc data is explicit.
  run_adb shell "observed=\$(ps -A -o PID,NAME)
pid=\$(printf '%s\\n' \"\$observed\" | awk '\$2 == \"echo-bridge\" { print \$1; exit }')
if test -n \"\$pid\"; then
  printf 'process=running\\n'
  users=\$(ps -A -o PID,USER,NAME 2>/dev/null) || users=''
  user=\$(printf '%s\\n' \"\$users\" | awk -v pid=\"\$pid\" '\$1 == pid && \$3 == \"echo-bridge\" { print \$2 }')
  case \$user in nobody | 9999 | root | 0) ;; *) user=unavailable ;; esac
  printf 'process_user=%s\\n' \"\$user\"
  age=unavailable
  case \$pid in *[!0-9]* | '') ;; *)
    if test -r /proc/\$pid/stat && test -r /proc/uptime && ticks=\$(getconf CLK_TCK 2>/dev/null); then
      age=\$(awk -v ticks=\"\$ticks\" 'NR == FNR { sub(/^.*\\) /, \"\"); start=\$20; next }
        ticks > 0 && start ~ /^[0-9]+$/ { age=int(\$1-start/ticks); if (age >= 0) print age }' /proc/\$pid/stat /proc/uptime)
      test -n \"\$age\" || age=unavailable
    fi ;;
  esac
  printf 'uptime_seconds=%s\\n' \"\$age\"
else printf 'process=stopped\\nprocess_user=unavailable\\nuptime_seconds=unavailable\\n'; fi
if test -f /data/local/hassmic/boot.log && test ! -L /data/local/hassmic/boot.log; then
  link=\$(awk '/client connected|no Home Assistant link/ { line=\$0 } END { print line }' /data/local/hassmic/boot.log)
  printf 'hassmic_link=%s\\n' \"\${link:-unavailable}\"
else printf 'hassmic_link=unavailable\\n'; fi
printf 'bridge.log (last 5):\\n'
if test -f '$device/private/bridge.log'; then tail -n 5 '$device/private/bridge.log';
else printf 'unavailable\\n'; fi"
  # Mask whole suspect lines, including arbitrary long credential-like words.
  # Buffer first: a failed filter must never cause a raw-output fallback.
  sanitized=$(awk '
    { lower=tolower($0); secret=(lower ~ /token|bearer|api[ _-]*key|password|secret|credential/)
      count=split($0, words, /[^a-zA-Z0-9_+.\/-]/)
      for (i=1; i<=count; i++) {
        if (length(words[i]) >= 24 || words[i] ~ /^eyJ.*\..*\./) secret=1
      }
      if (secret) print "[masked]"; else print
    }' "$log") || fail 'status filter failed'
  printf '%s\n' "$sanitized"
  case $sanitized in
    *'process_user=root'* | *'process_user=0'*) fail 'echo-bridge is running as root; stop and reinstall before starting' ;;
    *'process=running'*'process_user=unavailable'*) fail 'cannot verify echo-bridge process user' ;;
  esac
  exit 0
fi
run_adb shell "umask 077; mkdir '$lock'"
locked=true
# Reobserve after locking: a competing start may have completed during validation.
process_state
case $command_name in install | start) [ "$running" = 0 ] || fail 'bridge is already running; stop first' ;; esac
stop_bridge() {
  run_adb shell "$close_stdin"
  process_state
  [ "$running" = 0 ] || fail 'bridge did not stop; no process was signalled'
  run_adb shell "rm -rf '$device/private/run'"
}
if [ "$command_name" = stop ]; then
  stop_bridge
  printf '%s\n' 'echo-install: stopped'
  exit 0
fi
if [ "$command_name" = uninstall ]; then
  stop_bridge
  run_adb shell "rm -rf '$device/bin'
rm -f '$device/private/launch.sh'"
  if [ "$purge" = true ]; then
    run_adb shell "rm -rf '$device/private' '$device/home'"
    printf '%s\n' 'echo-install: uninstalled (purged)'
  else
    printf '%s\n' 'echo-install: uninstalled (state retained)'
  fi
  exit 0
fi
# Only stopped, fixed runtime roots are traversed; never follow links or accept
# FIFOs/devices/sockets as retained data. Validate the entire tree before changes.
migrate_state() {
  run_adb shell "for root in '$device/private' '$device/home'; do
  test ! -e \"\$root\" || test -d \"\$root\" || exit 1
  if test -d \"\$root\"; then
    bad=\$(find \"\$root\" ! -type d ! -type f -print)
    test -z \"\$bad\" || exit 1
  fi
done
for root in '$device/private' '$device/home'; do
  test -d \"\$root\" || continue
  find \"\$root\" -exec sh -c '
    for p do
      uid=\$(stat -c %u \"\$p\") || exit 1
      if test \"\$uid\" != 9999; then chown $ECHO_RUN_OWNER \"\$p\" || exit 1; fi
      mode=600
      if test -d \"\$p\" || test \"\$p\" = \"$device/private/launch.sh\"; then mode=700; fi
      actual=\$(stat -c %a \"\$p\") || exit 1
      if test \"\$actual\" != \"\$mode\"; then chmod \"\$mode\" \"\$p\" || exit 1; fi
    done' sh {} + || exit 1
done"
}
write_launcher() {
  run_adb shell "test ! -L '$device/private/launch.sh'
test ! -L '$device/private/bridge.log'
umask 077
cat > '$device/private/.install-stage' <<'LAUNCH'
#!/system/bin/sh
umask 077
state=/data/local/echo-bridge/private/run
# Atomic admission: cancellation may have consumed this gate first.
rmdir \"\$state/control\" 2>/dev/null || exit 1
if test -f \"\$state/stop\"; then touch \"\$state/finished\"; exit 1; fi
# Only this session holds the write end. The bridge must not inherit it.
exec 3<> \"\$state/stdin\"
/data/local/echo-bridge/bin/echo-bridge --config /data/local/echo-bridge/private/bridge.toml --trace /data/local/echo-bridge/private/bridge.trace < \"\$state/stdin\" 3>&- &
child=\$!
(while test ! -f \"\$state/stop\"; do sleep 1; done; exec 3>&-) &
holder=\$!
exec 3>&-
wait \"\$child\"
# This child is still ours and unreaped, so its PID cannot have been reused.
kill \"\$holder\" 2>/dev/null || :
wait \"\$holder\" 2>/dev/null || :
touch \"\$state/finished\"
LAUNCH
chown '$ECHO_RUN_OWNER' '$device/private/.install-stage' &&
chmod 700 '$device/private/.install-stage' &&
mv -f '$device/private/.install-stage' '$device/private/launch.sh' &&
touch '$device/private/bridge.log' && chown '$ECHO_RUN_OWNER' '$device/private/bridge.log' && chmod 600 '$device/private/bridge.log'"
}
if [ "$command_name" = start ]; then
  # Recover only owned state after proving no bridge is currently running.
  stop_bridge
  migrate_state
  safe_paths=true
  write_launcher
  run_adb shell "test -x '$device/private/launch.sh' && test -f '$device/private/bridge.toml'
test ! -e '$device/private/run'
umask 077
mkdir '$device/private/run'
trap \"rm -rf '$device/private/run'\" 0
mkdir '$device/private/run/control'
printf '%s\\n' \$((\$(date +%s) + 20)) > '$device/private/run/deadline'
mkfifo '$device/private/run/stdin'
# Record identity and the complete old prefix, never its contents.
stat -c %i /data/local/hassmic/boot.log > '$device/private/run/log-inode'
cksum < /data/local/hassmic/boot.log > '$device/private/run/log-prefix'
trap - 0"
  starting=true
  run_adb shell "chown -R '$ECHO_RUN_OWNER' '$device/private/run'"
  run_adb shell "setsid /data/local/hassmic/runas \"$run_user\" \"$run_groups\" /system/bin/sh '$device/private/launch.sh' </dev/null >> '$device/private/bridge.log' 2>&1 &"
  run_adb shell "
read -r end < '$device/private/run/deadline'
read -r checksum bytes < '$device/private/run/log-prefix'
read -r inode < '$device/private/run/log-inode'
while test \"\$(date +%s)\" -lt \"\$end\"; do
  test ! -f '$device/private/run/finished' || exit 1
  test \"\$(stat -c %i /data/local/hassmic/boot.log)\" = \"\$inode\" || exit 1
  prefix=\$(head -c \"\$bytes\" /data/local/hassmic/boot.log | cksum)
  test \"\$prefix\" = \"\$checksum \$bytes\" || exit 1
  if ps -A -o PID,NAME | awk '\$2 == \"echo-bridge\" { found=1 } END { exit !found }' &&
    tail -c +\$((bytes + 1)) /data/local/hassmic/boot.log | grep -q 'client connected'; then
    test \"\$(date +%s)\" -lt \"\$end\" || exit 1
    exit 0
  fi
  sleep 1
done
exit 1"
  starting=false
  printf '%s\n' 'echo-install: started (hassmic connected)'
  exit 0
fi
# Fixed destinations only. Refuse links before chmod, redirects, push or rename.
stop_bridge
run_adb shell "for p in '$device' '$device/bin' '$device/private' '$device/home' '$device/home/prompts'; do
  test ! -L \"\$p\" && { test ! -e \"\$p\" || test -d \"\$p\"; } || exit 1
done
umask 077
mkdir -p '$device/bin' '$device/private' '$device/home/prompts' &&
chown 0:0 '$device' '$device/bin' &&
chmod 755 '$device' '$device/bin'"
safe_paths=true
migrate_state

# Each copy is staged privately; failed transfers never replace an existing file.
# The destination leaf is fixed or a restricted prompt basename, never shell input.
copy_file() {
  local_file=$1 destination=$2 mode=$3 replace=$4
  owner=$ECHO_RUN_OWNER
  case $destination in "$device/bin/"*) owner=0:0 ;; esac
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
  run_adb shell "chown '$owner' '$device/private/.install-stage' && chmod '$mode' '$device/private/.install-stage' && mv -f '$device/private/.install-stage' '$destination'"
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
write_launcher
[ "$prompts" = true ] || printf '%s\n' 'echo-install: prompt defaults absent; skipped'
printf '%s\n' 'echo-install: installed'
