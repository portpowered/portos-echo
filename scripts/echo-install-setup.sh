#!/bin/sh
# Device-side setup support; lifecycle locking and output policy stay in the driver.
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
