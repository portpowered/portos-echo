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

# Each copy is staged privately; failed transfers never replace an existing file.
# Destinations are fixed or have a restricted prompt basename.
copy_file() {
  local_file=$1 destination=$2 mode=$3 replace=$4 digest=${5:-}
  owner=${ECHO_RUN_OWNER:?}
  case $destination in "${device:?}/bin/"*) owner=0:0 ;; esac
  run_adb shell "test ! -L '$destination' && { test ! -e '$destination' || test -f '$destination'; }"
  if [ "$replace" = false ]; then
    run_adb shell "if test -f '$destination'; then printf retained; else printf missing; fi"
    if [ "$(cat "${log:?}")" = retained ]; then
      run_adb shell "chmod '$mode' '$destination'"
      return 0
    fi
  fi
  stage_file=$device/private/.install-stage
  run_adb shell "test ! -e '$stage_file' && test ! -L '$stage_file'"
  run_adb push "$local_file" "$stage_file"
  if [ -n "$digest" ]; then
    run_adb shell "sum=\$(sha256sum < '$stage_file')
test \"\${sum%% *}\" = '$digest'"
  fi
  run_adb shell "chown '$owner' '$stage_file' && chmod '$mode' '$stage_file' && mv -f '$stage_file' '$destination'"
}

# SHA-256 of the release blobs at 3313a7da, e707712d and 218736fe, per basename.
historical_prompt_seed() {
  case $1:$2 in
    runner:0ab110ae3c6cd256eb01db6862b5d60db67fe2853bb9debb0ae0376a10daca04 | \
      runner:a18d535d17f290d80c2abb96cc36f4eb55912f7618370cf453224474d7db1bd4 | \
      runner:2656728cf9edbc981852fa484c49f4b27b90befb4f4421cba11cb61f2967297a | \
      voice:88821b161b9570cdd9a93663c3dff8fd75c85f633c8c36733eed5cbff5414212 | \
      voice:cb6f6bb7d6756a758e94beba11f91f5f9baa06f4fd7b27dd1619b73b15d92c34) return 0 ;;
    *) return 1 ;;
  esac
}

release_guard_prompts() {
  run_adb shell "seeds='${device:?}/private/prompt-seeds'
test ! -L \"\$seeds\" && { test ! -e \"\$seeds\" || test -d \"\$seeds\"; } || exit 1
for name in runner voice; do
  for leaf in '${device:?}/home/prompts/'\$name.md \"\$seeds/\$name.sha256\"; do
    test ! -L \"\$leaf\" && { test ! -e \"\$leaf\" || test -f \"\$leaf\"; } || exit 1
  done
done"
}

read_prompt_seed() {
  # The largest record holds two 64-digit digests and the three field labels.
  run_adb shell "bytes=0
if test -f '$seed_record'; then
  bytes=\$(stat -c %s '$seed_record')
fi
if test \"\$bytes\" -gt 0 && test \"\$bytes\" -le 156; then
  awk -v bytes=\"\$bytes\" '
    function digest(s) { return s == \"-\" || (length(s) == 64 && s ~ /^[0-9a-f]+\$/) }
    { size += length(\$0) + 1 }
    NR == 1 { version = (\$0 == \"version 1\") }
    NR == 2 && /^applied / { applied = substr(\$0, 9) }
    NR == 3 && /^pending / { pending = substr(\$0, 9) }
    END {
      if (version && NR == 3 && size == bytes && digest(applied) && digest(pending))
        print applied, pending
      else print \"- -\"
    }' '$seed_record'
else
  printf '%s\\n' '- -'
fi"
  read -r applied pending <"${log:?}"
}

write_prompt_seed() {
  run_adb shell "umask 077
if ! test -d '$seed_dir'; then
  mkdir '$seed_dir' && chown '${ECHO_RUN_OWNER:?}' '$seed_dir' || exit 1
fi
test ! -e '$device/private/.install-stage' && test ! -L '$device/private/.install-stage' || exit 1
printf '%s\\n' 'version 1' 'applied $1' 'pending $2' > '$device/private/.install-stage'
chown '$ECHO_RUN_OWNER' '$device/private/.install-stage' &&
chmod 600 '$device/private/.install-stage' &&
mv -f '$device/private/.install-stage' '$seed_record'"
}

reconcile_prompt() (
  prompt_name=$2
  prompt_destination=${device:?}/home/prompts/$prompt_name.md
  seed_dir=$device/private/prompt-seeds
  seed_record=$seed_dir/$prompt_name.sha256
  candidate=$(sha256sum <"$1") || fail 'required local input is not readable'
  candidate=${candidate%% *}
  read_prompt_seed
  run_adb shell "
if test -e '$prompt_destination'; then
  sha256sum < '$prompt_destination'
else
  printf '%s\\n' '-'
fi"
  read -r installed _ <"$log"
  if [ "$installed" = "$candidate" ]; then
    if [ "$applied" != "$candidate" ] || [ "$pending" != - ]; then
      write_prompt_seed "$candidate" -
    fi
    disposition=current
  elif [ "$installed" = - ] || [ "$installed" = "$applied" ] ||
    [ "$installed" = "$pending" ] || historical_prompt_seed "$prompt_name" "$installed"; then
    # Journal the seed still on disk and the verified replacement before the rename.
    write_prompt_seed "$installed" "$candidate"
    copy_file "$1" "$prompt_destination" 600 true "$candidate"
    write_prompt_seed "$candidate" -
    disposition=refreshed
    [ "$installed" != - ] || disposition=seeded
  else
    disposition=kept
  fi
  printf 'echo-install: prompt %s.md %s\n' "$prompt_name" "$disposition"
)

release_install_prompts() {
  prompts=false
  for prompt in "$1"/*.md; do
    [ -f "$prompt" ] || continue
    name=${prompt##*/}
    case $name in
      runner.md | voice.md) reconcile_prompt "$prompt" "${name%.md}" ;;
      *[!a-zA-Z0-9_.-]*) fail 'unsupported prompt filename' ;;
      *) copy_file "$prompt" "${device:?}/home/prompts/$name" 600 false ;;
    esac
    prompts=true
  done
  [ "$prompts" = true ] || printf '%s\n' 'echo-install: prompt defaults absent; skipped'
}
