#!/bin/sh
# Shell-owned release bookkeeping, sourced by the lifecycle driver.
release_stage_guard() {
  case $1 in /data/local/tmp/echo-setup-*) ;; *) return 1 ;; esac
  case ${1#/data/local/tmp/echo-setup-} in '' | *[!0-9-]*) return 1 ;; esac
  [ ! -L /data/local/tmp ] && [ ! -L "$1" ] && [ -d "$1" ] &&
    [ "$(stat -c %u:%a "$1")" = 0:700 ]
}

release_hash_valid() {
  [ "${#1}" = 64 ] || return 1
  case $1 in *[!0-9a-f]*) return 1 ;; esac
}

release_screen_pack() {
  [ "$#" = 5 ] && [ "$4" = --stage ] || exit 2
  stage=$5
  release_stage_guard "$stage" || exit 1
  id_file=$stage/screenpack.identity
  [ ! -L "$id_file" ] && [ -f "$id_file" ] && [ "$(stat -c %s "$id_file")" -le 255 ] || exit 1
  identity=$(cat "$id_file")
  case $identity in default@* | template@*) ;; *) exit 1 ;; esac
  release_version_valid "${identity#*@}" || exit 1
  device=/data/local/echo-bridge
  screens=$device/home/screens
  tree=$stage/pack/$identity
  destination=$screens/$identity
  marker=.$identity.sha256
  umask 077
  case $1:$2 in
    prepare-screen-pack:--sha256) screen_pack_prepare "$3" ;;
    publish-screen-pack:--identity)
      [ "$3" = "$identity" ] || exit 1
      screen_pack_publish
      ;;
    *) exit 2 ;;
  esac
}

screen_pack_prepare() {
  archive=$stage/screenpack.tar.gz
  release_hash_valid "$1" || return 1
  [ ! -L "$archive" ] && [ -f "$archive" ] && [ "$(stat -c %s "$archive")" -le 68157440 ] || return 1
  [ "$(sha256sum "$archive" | awk '{print $1}')" = "$1" ] || return 1

  # Bound both listings before using tar's reported member sizes to admit extraction.
  export LC_ALL=C
  (
    ulimit -f 128
    tar -tzf "$archive"
  ) >"$stage/pack.names" 2>/dev/null || return 1
  (
    ulimit -f 128
    tar -tvzf "$archive"
  ) >"$stage/pack.types" 2>/dev/null || return 1
  awk '
    /(^\/|\/\/|(^|\/)\.\.?($|\/)|[^A-Za-z0-9_.\/-])/ { bad=1 }
    !/\.(json|css|png|webp|webm|mp4)$/ || seen[$0]++ { bad=1 }
    END { exit (bad || NR > 64 || !seen["pack.json"]) }' "$stage/pack.names" || return 1
  awk '
    substr($0,1,1) != "-" || $3 !~ /^[0-9]+$/ { bad=1 }
    { total+=$3 }
    /\.(mp4|webm)$/ && $3 > 8388608 { bad=1 }
    END { exit (bad || total > 67108864) }' "$stage/pack.types" || return 1
  mkdir -p "$tree" || return 1
  tar -xzf "$archive" -C "$tree" --no-same-owner --no-same-permissions 2>/dev/null || return 1
  [ -f "$tree/pack.json" ] && [ "$(stat -c %s "$tree/pack.json")" -le 1048576 ] || return 1
  chown -R 9999:9999 "$tree" || return 1
  find "$tree" -type d -exec chmod 700 {} + || return 1
  find "$tree" -type f -exec chmod 600 {} + || return 1
  printf '%s' "$1" >"$stage/$marker" || return 1
  chown 9999:9999 "$stage/$marker" || return 1
  screen_pack_preflight || {
    : >"$stage/screenpack.conflict"
    return 1
  }
  cat "$tree/pack.json"
}

screen_pack_directory() {
  [ ! -L "$1" ] || return 1
  [ -e "$1" ] || return 0
  [ -d "$1" ] || return 1
  case $(stat -c %u:%g:%a "$1") in "$2:700" | "$2:755") ;; *) return 1 ;; esac
}

screen_pack_preflight() {
  screen_pack_directory "$device" 0:0 &&
    screen_pack_directory "$device/home" 9999:9999 &&
    screen_pack_directory "$screens" 9999:9999 &&
    screen_pack_directory "$destination" 9999:9999 || return 1
  pin=$screens/$marker
  [ ! -L "$pin" ] || return 1
  if [ -e "$pin" ]; then
    [ -f "$pin" ] && [ "$(stat -c %u:%g:%a "$pin")" = 9999:9999:600 ] &&
      [ "$(stat -c %h "$pin")" = 1 ] && [ "$(stat -c %s "$pin")" = 64 ] &&
      [ "$(cat "$pin")" = "$(cat "$stage/$marker")" ] || return 1
  fi
  if [ -d "$destination" ]; then
    screen_pack_inventory "$tree" source && screen_pack_inventory "$destination" installed &&
      [ "$(sha256sum <"$stage/source")" = "$(sha256sum <"$stage/installed")" ] || return 1
  fi
}

screen_pack_inventory() (
  cd "$1" || exit 1
  find . -exec sh -c '
    for path do
      case $path in *[!A-Za-z0-9_./-]*) exit 1 ;; esac
      test ! -L "$path" || exit 1
      if test -d "$path"; then
        case $(stat -c %u:%g:%a "$path") in 9999:9999:700 | 9999:9999:755) ;; *) exit 1 ;; esac
        printf "directory %s\n" "$path"
      else
        test -f "$path" && test "$(stat -c %h "$path")" = 1 || exit 1
        case $(stat -c %u:%g:%a "$path") in 9999:9999:600 | 9999:9999:644) ;; *) exit 1 ;; esac
        sha256sum "$path" || exit 1
      fi
    done' sh {} + >"$stage/$2.raw" || exit 1
  LC_ALL=C sort "$stage/$2.raw" >"$stage/$2"
)

screen_pack_publish() {
  # Serialize with install/start. Pack workers run only in the bridge process.
  lock=$device.lock
  mkdir "$lock" || return 1
  trap 'rmdir "$lock"' EXIT
  trap 'exit 1' HUP INT TERM
  processes=$(ps -A -o PID,NAME) || return 1
  printf '%s\n' "$processes" | awk '$2 == "echo-bridge" { busy=1 } END { exit busy+0 }' || return 1
  screen_pack_preflight || return 1
  if [ ! -d "$screens" ]; then
    mkdir "$screens" && chown 9999:9999 "$screens" || return 1
  fi
  stage_dev=$(stat -c %d "$tree") && screens_dev=$(stat -c %d "$screens") || return 1
  [ "$stage_dev" = "$screens_dev" ] || return 1
  if [ ! -e "$screens/$marker" ]; then
    mv -n "$stage/$marker" "$screens/" && [ ! -e "$stage/$marker" ] || return 1
  fi
  if [ ! -d "$destination" ]; then
    mv -n "$tree" "$screens/" && [ ! -e "$tree" ] || return 1
  fi
}

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
