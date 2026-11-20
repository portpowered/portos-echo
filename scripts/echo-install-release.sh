#!/bin/sh
# Shell-owned release bookkeeping, sourced by the lifecycle driver.
release_metadata() {
  run_adb shell "if test -e '${device:?}/release.json' || test -L '${device:?}/release.json'; then
  test ! -L '${device:?}/release.json' && test -f '${device:?}/release.json' || exit 1
  test \"\$(stat -c %u '${device:?}')\" = 0 && test \"\$(stat -c %a '${device:?}')\" = 755 || exit 1
  test \"\$(stat -c %u '${device:?}/release.json')\" = 0 && test \"\$(stat -c %h '${device:?}/release.json')\" = 1 || exit 1
  case \$(stat -c %a '${device:?}/release.json') in 600 | 644) ;; *) exit 1 ;; esac
  test \"\$(stat -c %s '${device:?}/release.json')\" -le 1024 || exit 1
  awk '
    function version(v, parts, n, i) {
      if (v == \"null\") return 1
      if (v !~ /^\"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[A-Za-z0-9]+(\.[A-Za-z0-9]+)*)?\"$/) return 0
      sub(/^.*-/, \"\", v); sub(/\"$/, \"\", v)
      n=split(v, parts, /[.]/)
      for (i=1; i<=n; i++) if (parts[i] ~ /^0[0-9]+$/) return 0
      return 1
    }
    { data=data \$0 \"\\n\" }
    END {
      if (data !~ /^[ \t\r\n]*\{[ \t\r\n]*\"format\"[ \t\r\n]*:[ \t\r\n]*1[ \t\r\n]*,[ \t\r\n]*\"current\"[ \t\r\n]*:[ \t\r\n]*(null|\"[A-Za-z0-9.-]+\")[ \t\r\n]*,[ \t\r\n]*\"previous\"[ \t\r\n]*:[ \t\r\n]*(null|\"[A-Za-z0-9.-]+\")[ \t\r\n]*\}[ \t\r\n]*$/) exit 1
      gsub(/[ \t\r\n{}]/, \"\", data); split(data, fields, \",\")
      sub(/^\"current\":/, \"\", fields[2]); sub(/^\"previous\":/, \"\", fields[3])
      if (!version(fields[2]) || !version(fields[3])) exit 1
      gsub(/\"/, \"\", fields[2]); gsub(/\"/, \"\", fields[3])
      printf \"version_current=%s\\nversion_previous=%s\\n\", fields[2], fields[3]
    }' '${device:?}/release.json'
fi"
}

release_guard() {
  run_adb shell "for root in '${device:?}/bin' '${device:?}/apk' '${device:?}/scripts' '${device:?}/releases'; do
  test ! -L \"\$root\" || exit 1
  test -e \"\$root\" || continue
  test -d \"\$root\" || exit 1
  bad=\$(find \"\$root\" ! -type d ! -type f -print)
  test -z \"\$bad\" || exit 1
  dirs=\$(find \"\$root\" -type d -exec stat -c %u:%a {} +) || exit 1
  files=\$(find \"\$root\" -type f -exec stat -c %u:%h:%a {} +) || exit 1
  test -n \"\$dirs\" || exit 1
  printf '%s\\n' \"\$dirs\" | awk 'NF && \$0 !~ /^0:(600|644|700|755)$/ { bad=1 } END { exit bad+0 }'
  printf '%s\\n' \"\$files\" | awk 'NF && \$0 !~ /^0:1:(600|644|700|755)$/ { bad=1 } END { exit bad+0 }'
done
test ! -L '${device:?}/MANIFEST'
if test -e '${device:?}/MANIFEST'; then
  test -f '${device:?}/MANIFEST' || exit 1
  info=\$(stat -c %u:%h:%a '${device:?}/MANIFEST')
  case \$info in 0:1:600 | 0:1:644) ;; *) exit 1 ;; esac
fi"
  case ${command_name:?} in install | rollback | stop | uninstall) return ;; esac
  run_adb shell "test ! -e '${device:?}/releases/pending'"
}
release_version_field() {
  printf '%s\n' "${versions-}" | awk -F= -v field="version_$1" '$1 == field { print $2; found=1 } END { if (!found) print "null" }'
}

release_snapshot() {
  # A same-version refresh needs recovery bytes without replacing the rollback slot.
  snapshot=previous
  active_version=$(release_version_field current)
  if [ "${release_version:?}" != null ] && [ "$release_version" = "$active_version" ]; then
    snapshot=retry
  fi
  run_adb shell "umask 077
mkdir -p '${device:?}/releases'
chown 0:0 '${device:?}/releases'
chmod 700 '${device:?}/releases'
if test ! -e '${device:?}/releases/previous' && test -d '${device:?}/releases/retired'; then
  mv '${device:?}/releases/retired' '${device:?}/releases/previous'
fi
rm -rf '${device:?}/releases/stage'
mkdir '${device:?}/releases/stage'
for name in bin apk scripts MANIFEST; do
  if test -e '${device:?}/'\$name; then cp -R '${device:?}/'\$name '${device:?}/releases/stage/'\$name; fi
done
printf '%s\\n' '$active_version' > '${device:?}/releases/stage/version'
cd '${device:?}/releases/stage'
find . -type f ! -path ./SHA256SUMS -exec sha256sum {} + | sort > SHA256SUMS
sha256sum -c SHA256SUMS >/dev/null 2>&1
chown -R 0:0 '${device:?}/releases/stage'
if test '$snapshot' = retry; then
  rm -rf '${device:?}/releases/retry'
elif test -d '${device:?}/releases/previous'; then
  rm -rf '${device:?}/releases/retired'
  mv '${device:?}/releases/previous' '${device:?}/releases/retired'
fi
mv '${device:?}/releases/stage' '${device:?}/releases/$snapshot'
rm -rf '${device:?}/releases/retired'
printf '%s\\n' '$snapshot' > '${device:?}/releases/transaction'
mv -f '${device:?}/releases/transaction' '${device:?}/releases/pending'
rm -f '${device:?}/private/launch.sh'"
}
release_publish() {
  if [ -n "${release_bundle-}" ]; then
    run_adb shell "for name in apk scripts MANIFEST; do
  test ! -L '${release_bundle-}/'\$name || exit 1
  rm -rf '${device:?}/'\$name
  cp -R '${release_bundle-}/'\$name '${device:?}/'\$name
done
chown -R 0:0 '${device:?}/apk' '${device:?}/scripts' '${device:?}/MANIFEST'
chmod 755 '${device:?}/apk' '${device:?}/scripts'
chmod 644 '${device:?}/apk/echo-screen.apk' '${device:?}/scripts/echo-install.sh' '${device:?}/scripts/echo-install-release.sh' '${device:?}/scripts/echo-install-setup.sh' '${device:?}/MANIFEST'"
  fi
  current=${release_version:?}
  previous=$active_version
  if [ "$snapshot" = retry ]; then
    previous=$(release_version_field previous)
  fi
  release_write_metadata
}
release_write_metadata() {
  [ "$current" = null ] || current=\"$current\"
  [ "$previous" = null ] || previous=\"$previous\"
  run_adb shell "umask 077
printf '%s\\n' '{\"format\":1,\"current\":$current,\"previous\":$previous}' > '${device:?}/releases/metadata'
chown 0:0 '${device:?}/releases/metadata'
chmod 644 '${device:?}/releases/metadata'
mv -f '${device:?}/releases/metadata' '${device:?}/release.json'
rm -f '${device:?}/releases/pending'
rm -rf '${device:?}/releases/retry'"
}
release_verify_previous() {
  run_adb shell "for name in echo-bridge codex-app-server cacert.pem; do
  test -f '${device:?}/releases/previous/bin/'\$name || exit 1
done"
  release_verify_snapshot previous
}
release_verify_snapshot() {
  run_adb shell "cd '${device:?}/releases/$1'
for name in version SHA256SUMS; do
  test -f \"\$name\" || exit 1
done
find . -type f ! -path ./SHA256SUMS -exec sha256sum {} + | sort > ../observed
cmp SHA256SUMS ../observed >/dev/null 2>&1
rm ../observed
sha256sum -c SHA256SUMS >/dev/null 2>&1
value=\$(cat version)
printf '%s\\n' \"\$value\""
}
release_restore_files() {
  # Select the verified restore target before changing any active files.
  run_adb shell "umask 077
printf '%s\\n' '$1' > '${device:?}/releases/transaction'
mv -f '${device:?}/releases/transaction' '${device:?}/releases/pending'
for name in bin apk scripts MANIFEST; do
  rm -rf '${device:?}/'\$name
  if test -e '${device:?}/releases/$1/'\$name; then
    cp -R '${device:?}/releases/$1/'\$name '${device:?}/'\$name
  fi
done
chown -R 0:0 '${device:?}/bin'
if test -f '${device:?}/bin/echo-bridge'; then
  chmod 755 '${device:?}/bin' '${device:?}/bin/echo-bridge' '${device:?}/bin/codex-app-server'
  chmod 644 '${device:?}/bin/cacert.pem'
fi"
}
release_restore() {
  release_restore_files previous
  current=$saved_version
  previous=null
  release_write_metadata
  run_adb shell "rm -rf '${device:?}/releases/previous'"
}

release_version_valid() {
  printf '%s\n' "$1" | awk '
    /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[A-Za-z0-9]+(\.[A-Za-z0-9]+)*)?$/ {
      if (index($0, "-")) {
        sub(/^.*-/, ""); n=split($0, parts, /[.]/)
        for (i=1; i<=n; i++) if (parts[i] ~ /^0[0-9]+$/) exit 1
      }
      valid=1
    }
    END { exit !valid || NR != 1 }'
}

release_recover() {
  run_adb shell "if test -f '${device:?}/releases/pending'; then
  if test -s '${device:?}/releases/pending'; then cat '${device:?}/releases/pending'; else printf previous; fi
fi"
  pending=$(cat "${log:?}")
  case $pending in
    '') return ;;
    previous | retry) ;;
    *) fail 'invalid release recovery marker' ;;
  esac
  release_verify_snapshot "$pending"
  saved_version=$(cat "${log:?}")
  [ "$saved_version" = null ] || release_version_valid "$saved_version" || fail 'invalid previous release'
  if [ "$pending" = previous ]; then
    release_restore
  else
    release_restore_files retry
    current=$saved_version
    previous=$(release_version_field previous)
    release_write_metadata
  fi
  release_metadata
  versions=$(cat "${log:?}")
}
