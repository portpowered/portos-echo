#!/bin/sh
# Device-side setup support; lifecycle locking and output policy stay in the driver.
setup_observation() {
  case ${1:-} in
    setup-config | setup-pairing)
      setup_private_read "$@" 2>/dev/null || exit 1
      exit 0
      ;;
    wake-cursor | wake-check)
      setup_wake "$@" 2>/dev/null || exit 1
      exit 0
      ;;
    confirm-cursor | confirm-check)
      setup_confirmation "$@" 2>/dev/null || exit 1
      exit 0
      ;;
  esac
}

setup_private_read() {
  # Private transport only: never dispatch these reads as public CLI output.
  [ "$#" = 1 ] || return 1
  base=/data/local/echo-bridge
  test ! -L "$base" && test -d "$base" || return 1
  test ! -L "$base/private" && test -d "$base/private" || return 1
  [ "$(stat -c %u:%a "$base/private")" = 9999:700 ] || return 1
  case $1 in
    setup-config)
      file=$base/private/bridge.toml
      limit=1048576
      ;;
    setup-pairing)
      file=$base/private/screen-pairing.txt
      limit=16384
      ;;
    *) return 1 ;;
  esac
  test ! -L "$file" && test -f "$file" || return 1
  [ "$(stat -c %u:%h:%a "$file")" = 9999:1:600 ] || return 1
  bytes=$(wc -c <"$file") || return 1
  [ "$bytes" -gt 0 ] && [ "$bytes" -le "$limit" ] || return 1
  cat "$file"
}

setup_wake() {
  # Only fixed projections escape these private log observers.
  wake_log=/data/local/hassmic/boot.log
  wake_parent=/data/local/hassmic
  case $1 in run-*)
    wake_parent=/data/local/echo-bridge/private
    wake_log=$wake_parent/bridge.trace
    test ! -L /data/local/echo-bridge || return 1
    test "$(stat -c %u:%h:%a "$wake_log")" = 9999:1:600 || return 1
    ;;
  esac
  test ! -L "$wake_parent" && test -d "$wake_parent" || return 1
  test ! -L "$wake_log" && test -f "$wake_log" || return 1
  identity=$(stat -c %d:%i "$wake_log") || return 1
  if [ "$1" = wake-cursor ] || [ "$1" = run-cursor ]; then
    [ "$#" = 1 ] || return 1
    bytes=$(wc -c <"$wake_log") || return 1
    # Never let an old partial record become this attempt's first wake.
    if [ "$bytes" != 0 ]; then
      [ "$(head -c "$bytes" "$wake_log" | tail -c 1 | wc -l)" = 1 ] || return 1
    fi
    digest=$(head -c "$bytes" "$wake_log" | sha256sum | awk '{print $1}') || return 1
    cursor=$identity:$bytes:$digest
  else
    [ "$#" = 2 ] || return 1
    cursor=$2
  fi
  # Validate every field before arithmetic, comparison or shell interpolation.
  case $cursor in '' | *[!0-9a-f:]*) return 1 ;; esac
  printf '%s\n' "$cursor" | awk -F: 'NF != 4 { exit 1 }
    { for (i=1; i<=3; i++) if ($i !~ /^(0|[1-9][0-9]*)$/ || length($i)>15) exit 1
      if (length($4)!=64 || $4 ~ /[^0-9a-f]/) exit 1 }' || return 1
  old_identity=${cursor%:*:*} rest=${cursor#*:*:}
  bytes=${rest%%:*} digest=${rest#*:}
  [ "$identity" = "$old_identity" ] || return 1
  setup_wake_prefix || return 1
  if [ "$1" = wake-check ] || [ "$1" = run-check ]; then
    evidence=$(setup_log_evidence "$1") || return 1
    setup_wake_prefix || return 1
    printf '%s\n' "$evidence"
  else
    printf '%s\n' "$cursor"
  fi
}

setup_log_evidence() {
  # A trailing sentinel invalidates an incomplete suffix, even after evidence.
  if [ "$1" = run-check ]; then
    { tail -c +$((bytes + 1)) "$wake_log" && printf '\001'; } | awk '
      index($0, "\001") { complete=1; if (index($0, "\001")>1) run=0; next }
      function number(value) { return length(value)<20 || (length(value)==20 && "x" value <= "x18446744073709551615") }
      /^\{"elapsed_ms":(0|[1-9][0-9]*),"event":"wyoming\.run_started","provider_auth_mode":"(chatgpt|api_key)","run_id":"run-[1-9][0-9]*"\}$/ {
        elapsed=$0; sub(/^\{"elapsed_ms":/, "", elapsed); sub(/,.*/, "", elapsed)
        sequence=$0; sub(/^.*"run_id":"run-/, "", sequence); sub(/"\}$/, "", sequence)
        if (number(elapsed) && number(sequence)) run=1 }
      END { if (!complete) exit 1; printf "run=%s\n", run ? "started" : "pending" }'
  else
    { tail -c +$((bytes + 1)) "$wake_log" && printf '\001'; } | awk '
      index($0, "\001") { complete=1; if (index($0, "\001")>1) wake=linked=0; next }
      /(^|[[:space:]])wake: COMPUTER type=2([[:space:]]|$)/ { wake=1 }
      wake && /satellite: running([[:space:]]|$)/ { linked=1 }
      /client disconnected|no Home Assistant link/ { linked=0 }
      END { if (!complete) exit 1
        printf "wake=%s\nlink=%s\n", wake ? "confirmed" : "pending", linked ? "connected" : "pending" }'
  fi
}

setup_wake_prefix() {
  test ! -L "$wake_parent" && test ! -L "$wake_log" && test -f "$wake_log" || return 1
  [ "$(stat -c %d:%i "$wake_log")" = "$old_identity" ] || return 1
  [ "$(wc -c <"$wake_log")" -ge "$bytes" ] || return 1
  [ "$(head -c "$bytes" "$wake_log" | sha256sum | awk '{print $1}')" = "$digest" ]
}

setup_confirmation() {
  if [ "$1" = confirm-cursor ]; then
    [ "$#" = 1 ] || return 1
    wake=$(setup_wake wake-cursor) || return 1
    run=$(setup_wake run-cursor) || return 1
    # Detect replacement/rewrite during the paired capture too.
    setup_wake wake-check "$wake" >/dev/null || return 1
    setup_wake run-check "$run" >/dev/null || return 1
    printf '%s\n%s\n' "$wake" "$run"
  else
    [ "$#" = 3 ] || return 1
    wake=$(setup_wake wake-check "$2") || return 1
    run=$(setup_wake run-check "$3") || return 1
    printf '%s\n%s\n' "$wake" "$run"
  fi
}

setup_doctor() {
  # Read-only, bounded probes. Only fixed labels and numeric observations escape.
  run_adb shell "
if test -x /data/local/hassmic/runas; then
  printf 'hassmic=ok\\n'
  if /data/local/hassmic/runas \"${run_user:?}\" \"${run_groups:?}\" nc -z -w 5 chatgpt.com 443 >/dev/null 2>&1; then
    printf 'egress=ok\\n'
  else printf 'egress=failed\\n'; fi
else printf 'hassmic=failed\\negress=failed\\n'; fi
epoch=\$(date +%s 2>/dev/null) || epoch=unavailable
case \$epoch in '' | *[!0-9]*) epoch=unavailable ;; esac
printf 'clock=%s\\n' \"\$epoch\"
free=\$(df -k /data/local 2>/dev/null | awk 'NR > 1 { value=\$4 } END { print value }') || free=unavailable
case \$free in '' | *[!0-9]*) free=unavailable ;; esac
printf 'disk_free=%s\\n' \"\$free\"
users=\$(ps -A -o PID,USER,NAME 2>/dev/null) || users=''
user=\$(printf '%s\\n' \"\$users\" | awk '\$3 == \"echo-bridge\" { count++; user=\$2 } END { if (count == 1) print user }')
case \$user in nobody | 9999 | root | 0) ;; *) user=unavailable ;; esac
printf 'process_user=%s\\n' \"\$user\""
}

setup_operator() {
  if [ "${command_name:?}" = screen-setup ]; then
    setup_screen
    printf '%s\n' 'echo-install: screen ready'
    return
  fi
  case ${command_name:?} in api-key | api-key-status)
    [ "${running:?}" = 0 ] || fail 'stop the bridge before changing the API key'
    migrate_state
    if [ "$command_name" = api-key ]; then setup_api_key; else setup_api_key_status; fi
    return
    ;;
  esac
  case ${command_name:?} in
    signin | ensure-signin)
      [ "${running:?}" = 0 ] || fail 'stop the bridge before signing in'
      migrate_state
      ;;
    link)
      run_adb shell "users=\$(ps -A -o PID,USER,NAME)
printf '%s\\n' \"\$users\" | awk '\$3 == \"echo-bridge\" { count++; if (\$2 != \"nobody\" && \$2 != 9999) bad=1 } END { exit count != 1 || bad }'"
      ;;
  esac
  operator_command=$command_name
  [ "$operator_command" != ensure-signin ] || operator_command=signin
  # The bridge owns authentication and its screen presentation. Suppress all of
  # its terminal bytes; the native caller observes only authenticated screen SSE.
  run_adb shell "test -x '${device:?}/bin/echo-bridge'
test -f '${device:?}/private/bridge.toml'
test ! -L '${device:?}/private/bridge.toml'
test \"\$(stat -c %u:%h:%a '${device:?}/private/bridge.toml')\" = 9999:1:600
if test '${command_name:?}' = ensure-signin; then
  # The existing account owner checks its retained home. A successful status
  # must not reset the grant or open another sign-in screen. Neither the
  # account label nor any dependency output crosses the private transport.
  if /data/local/hassmic/runas \"${run_user:?}\" \"${run_groups:?}\" '${device:?}/bin/echo-bridge' status --config '${device:?}/private/bridge.toml' </dev/null >/dev/null 2>&1; then
    exit 0
  fi
fi
/data/local/hassmic/runas \"${run_user:?}\" \"${run_groups:?}\" '${device:?}/bin/echo-bridge' '${operator_command:?}' --config '${device:?}/private/bridge.toml' </dev/null"
  printf 'echo-install: %s completed\n' "$command_name"
}

setup_screen() {
  # The shell owns Android package/home selection for every native host.
  # Reapply home selection after an interrupted setup, even if pm installed it.
  run_adb shell "test ! -L /data/local/hassmic && test -d /data/local/hassmic/state
test ! -L /data/local/hassmic/state
for leaf in screen_home .echo-screen-home; do
  test ! -L /data/local/hassmic/state/\$leaf
  test ! -e /data/local/hassmic/state/\$leaf || test -f /data/local/hassmic/state/\$leaf
  test ! -e /data/local/hassmic/state/\$leaf || test \"\$(stat -c %h /data/local/hassmic/state/\$leaf)\" = 1
done
test -f '${device:?}/apk/echo-screen.apk'
# Version 2 is the first release declaring CAMERA. Keep same-signature upgrades in place.
packages=\$(pm list packages --show-versioncode org.portos.echoscreen)
if ! printf '%s\\n' \"\$packages\" | awk '\$1 == \"package:org.portos.echoscreen\" && \$2 ~ /^versionCode:[0-9]+\$/ { split(\$2, parts, \":\"); current = parts[2] >= 2 } END { exit !current }'; then
  pm install -r '${device:?}/apk/echo-screen.apk' >/dev/null
fi
pm grant org.portos.echoscreen android.permission.CAMERA >/dev/null
umask 077
printf '%s\\n' org.portos.echoscreen > /data/local/hassmic/state/.echo-screen-home
chown system:system /data/local/hassmic/state/.echo-screen-home
chmod 644 /data/local/hassmic/state/.echo-screen-home
mv -f /data/local/hassmic/state/.echo-screen-home /data/local/hassmic/state/screen_home
launched=\$(am start -n org.portos.echoscreen/.MainActivity 2>&1)
case \"\$launched\" in
  Error:* | *\"
Error:\"*) exit 1 ;;
esac"
}

setup_api_key_arguments() {
  # Fixed shape only: the root-only setup stage and one plain private leaf.
  [ "$#" = 5 ] && [ "$2" = --source ] && [ "$4" = --file ] || exit 2
  api_key_source=$3 api_key_file=$5
  case $api_key_source in /data/local/tmp/echo-setup-*/api-key) ;; *) exit 1 ;; esac
  api_key_stage=${api_key_source%/api-key}
  case ${api_key_stage#/data/local/tmp/echo-setup-} in '' | *[!0-9-]*) exit 1 ;; esac
  api_key_leaf=${api_key_file#/data/local/echo-bridge/private/}
  [ "$api_key_leaf" != "$api_key_file" ] && [ "${#api_key_leaf}" -le 64 ] || exit 1
  case $api_key_leaf in
    '' | .* | *[!A-Za-z0-9._-]* | bridge.toml | bridge.log | bridge.trace | screen-pairing.txt | launch.sh) exit 1 ;;
  esac
}

setup_api_key() {
  # The key moves from the root-only stage to the bridge user's private file.
  # Its bytes never reach argv, the log or any output.
  safe_paths=true
  run_adb shell "source='${api_key_source:?}' target='${api_key_file:?}' stage='${device:?}/private/.install-stage'
test ! -L '${api_key_stage:?}' && test -d '${api_key_stage:?}' || exit 1
test \"\$(stat -c %u:%a '${api_key_stage:?}')\" = 0:700 || exit 1
test ! -L \"\$source\" && test -f \"\$source\" || exit 1
bytes=\$(wc -c < \"\$source\")
test \"\$bytes\" -gt 0 && test \"\$bytes\" -le 16384 || exit 1
test ! -L \"\$target\" && { test ! -e \"\$target\" || test -f \"\$target\"; } || exit 1
test ! -e \"\$stage\" && test ! -L \"\$stage\" || exit 1
umask 077
cat \"\$source\" > \"\$stage\" || exit 1
rm -f \"\$source\"
chown '${ECHO_RUN_OWNER:?}' \"\$stage\" && chmod 600 \"\$stage\" && mv -f \"\$stage\" \"\$target\""
  printf '%s\n' 'echo-install: api-key completed'
}

setup_api_key_status() {
  # Only the fixed mode line is examined; the private log is never printed.
  run_adb shell "test -x '${device:?}/bin/echo-bridge'
test ! -L '${device:?}/private/bridge.toml'
test \"\$(stat -c %u:%h:%a '${device:?}/private/bridge.toml')\" = 9999:1:600
/data/local/hassmic/runas \"${run_user:?}\" \"${run_groups:?}\" '${device:?}/bin/echo-bridge' status --config '${device:?}/private/bridge.toml' </dev/null 2>/dev/null"
  awk '$0 == "mode: api-key" { found=1 } END { exit !found }' "${log:?}" || fail 'bridge did not report API-key mode'
  printf '%s\n' 'echo-install: api-key-status completed'
}
