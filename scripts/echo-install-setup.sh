#!/bin/sh
# Device-side setup support; lifecycle locking and output policy stay in the driver.
setup_observation() {
  case ${1:-} in
    wake-cursor | wake-check)
      setup_wake "$@" 2>/dev/null || exit 1
      exit 0
      ;;
  esac
}

setup_wake() {
  # Only this private transport observes boot.log; never return any log content.
  wake_log=/data/local/hassmic/boot.log
  test ! -L /data/local/hassmic && test -d /data/local/hassmic || return 1
  test ! -L "$wake_log" && test -f "$wake_log" || return 1
  identity=$(stat -c %d:%i "$wake_log") || return 1
  if [ "$1" = wake-cursor ]; then
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
  if [ "$1" = wake-check ]; then
    # The sentinel makes awk discard an unterminated final record. Evidence is
    # ordered: a link before the confirmed wake cannot complete this attempt.
    evidence=$({
      tail -c +$((bytes + 1)) "$wake_log" && printf '\001'
    } | awk '
      index($0, "\001") { complete=1; next }
      /(^|[[:space:]])wake: COMPUTER type=2([[:space:]]|$)/ { wake=1 }
      wake && /satellite: running([[:space:]]|$)/ { linked=1 }
      END { if (!complete) exit 1
        printf "wake=%s\nlink=%s\n", wake ? "confirmed" : "pending", linked ? "connected" : "pending" }') || return 1
    setup_wake_prefix || return 1
    printf '%s\n' "$evidence"
  else
    printf '%s\n' "$cursor"
  fi
}

setup_wake_prefix() {
  test ! -L /data/local/hassmic && test ! -L "$wake_log" && test -f "$wake_log" || return 1
  [ "$(stat -c %d:%i "$wake_log")" = "$old_identity" ] || return 1
  [ "$(wc -c <"$wake_log")" -ge "$bytes" ] || return 1
  [ "$(head -c "$bytes" "$wake_log" | sha256sum | awk '{print $1}')" = "$digest" ]
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
    signin)
      [ "${running:?}" = 0 ] || fail 'stop the bridge before signing in'
      migrate_state
      ;;
    link)
      run_adb shell "users=\$(ps -A -o PID,USER,NAME)
printf '%s\\n' \"\$users\" | awk '\$3 == \"echo-bridge\" { count++; if (\$2 != \"nobody\" && \$2 != 9999) bad=1 } END { exit count != 1 || bad }'"
      ;;
  esac
  # The bridge owns authentication and its screen presentation. Suppress all of
  # its terminal bytes; the native caller observes only authenticated screen SSE.
  run_adb shell "test -x '${device:?}/bin/echo-bridge'
test -f '${device:?}/private/bridge.toml'
test ! -L '${device:?}/private/bridge.toml'
test \"\$(stat -c %u:%h:%a '${device:?}/private/bridge.toml')\" = 9999:1:600
/data/local/hassmic/runas \"${run_user:?}\" \"${run_groups:?}\" '${device:?}/bin/echo-bridge' '${command_name:?}' --config '${device:?}/private/bridge.toml' </dev/null"
  printf 'echo-install: %s completed\n' "$command_name"
}
