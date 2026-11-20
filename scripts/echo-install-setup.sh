#!/bin/sh
# Device-side setup support; lifecycle locking and output policy stay in the driver.
setup_observation() {
  case ${1:-} in
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
      /^\{"elapsed_ms":(0|[1-9][0-9]*),"event":"wyoming\.run_started","provider_auth_mode":"(chatgpt|api_key)","run_id":"run-[1-9][0-9]*"\}$/ { run=1 }
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
  if /data/local/hassmic/runas nobody 3990,inet nc -z -w 5 chatgpt.com 443 >/dev/null 2>&1; then
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
