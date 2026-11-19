#!/bin/sh
# Paired release entry point. Parse the entire script before reading piped input.
main() {
  set -eu
  umask 077
  version='@ECHO_VERSION@'
  config=''
  seen=' '
  while [ "$#" -gt 0 ]; do
    flag=$1
    case $seen in *" $flag "*) usage ;; esac
    seen="$seen$flag "
    case $flag in
      --help)
        [ "$#" = 1 ] || usage
        printf '%s\n' 'Usage: sh install.sh [--version VERSION] [--config FILE]' \
          'Linux; requires curl or wget, tar, sha256sum, adb and ordinary coreutils.'
        exit 0
        ;;
      --version | --config)
        [ "$#" -ge 2 ] || usage
        case $2 in '' | --*) usage ;; esac
        case $flag in --version) version=${2#v} ;; --config) config=$2 ;; esac
        shift
        ;;
      *) usage ;;
    esac
    shift
  done
  printf '%s\n' "$version" | awk '
    NR != 1 { bad=1 }
    /^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9]+(\.[A-Za-z0-9]+)*)?$/ { ok=1 }
    END { exit (!ok || bad) }' || usage
  if [ -n "$config" ]; then
    [ -f "$config" ] && [ -r "$config" ] || fail 'config is not readable'
  fi
  for tool in tar sha256sum adb mktemp awk rm; do
    command -v "$tool" >/dev/null 2>&1 || fail 'required installer utility is missing'
  done
  if command -v curl >/dev/null 2>&1; then
    downloader=curl
  elif command -v wget >/dev/null 2>&1; then
    downloader=wget
  else fail 'curl or wget is required'; fi
  temp=$(mktemp -d) || fail 'private temporary directory creation failed'
  trap 'rm -rf -- "$temp"' 0
  trap 'exit 1' HUP INT TERM
  archive="echo-$version-armv7.tar.gz"
  base="https://github.com/portpowered/portos-echo/releases/download/v$version"
  download "$base/$archive" "$temp/$archive"
  download "$base/SHA256SUMS" "$temp/SHA256SUMS"
  # Accept exactly one strict checksum entry for this archive, never shell input.
  digest=$(awk -v name="$archive" '
    $2 == name { count++; if (NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-f]/) bad=1; hash=$1 }
    END { if (count != 1 || bad) exit 1; print hash }' "$temp/SHA256SUMS") || fail 'invalid archive checksum'
  (cd "$temp" && printf '%s  %s\n' "$digest" "$archive" | sha256sum -c -) >/dev/null 2>&1 || fail 'archive checksum mismatch'
  # No links, directories, special files, duplicates, whitespace or extension paths.
  tar -tzf "$temp/$archive" >"$temp/names" 2>/dev/null || fail 'unsafe archive'
  awk '
    /(^\/|(^|\/)\.\.?($|\/)|[^A-Za-z0-9_.\/-])/ { bad=1 }
    !/^(MANIFEST|LICENSE|NOTICE|bin\/(echo-bridge|codex-app-server|cacert.pem)|apk\/echo-screen.apk|scripts\/echo-install.sh|examples\/(bridge.echo.toml|prompts\/(voice|runner).md)|licenses\/[A-Za-z0-9_.\/-]+)$/ { bad=1 }
    { if (seen[$0]++) bad=1 }
    END { exit bad }' "$temp/names" || fail 'unsafe archive'
  tar -tvzf "$temp/$archive" >"$temp/types" 2>/dev/null || fail 'unsafe archive'
  awk 'substr($0,1,1) != "-" { bad=1 } END { exit bad }' "$temp/types" || fail 'unsafe archive'
  mkdir "$temp/bundle"
  tar -xzf "$temp/$archive" -C "$temp/bundle" --no-same-owner --no-same-permissions 2>/dev/null || fail 'archive extraction failed'
  bundle=$temp/bundle
  for file in scripts/echo-install.sh bin/echo-bridge bin/codex-app-server bin/cacert.pem apk/echo-screen.apk examples/bridge.echo.toml; do
    [ -f "$bundle/$file" ] || fail 'required bundle file is missing'
  done
  [ -n "$config" ] || config=$bundle/examples/bridge.echo.toml
  # Read-only prerequisite gives a clearer message than the underlying installer.
  adb devices >"$temp/devices" 2>/dev/null || fail 'adb device selection failed'
  awk 'NR > 1 && NF { count++; if ($2 == "device") ready++ }
    END { exit !(count == 1 && ready == 1) }' "$temp/devices" || fail 'exactly one ready adb device is required'
  adb shell 'test -x /data/local/hassmic/runas' >/dev/null 2>&1 || fail 'hassmic must already be installed'
  sh "$bundle/scripts/echo-install.sh" install --config "$config" \
    --bridge "$bundle/bin/echo-bridge" --codex "$bundle/bin/codex-app-server" \
    --ca "$bundle/bin/cacert.pem" >/dev/null 2>&1 || fail 'install failed; device state may be partial'
  sh "$bundle/scripts/echo-install.sh" start >/dev/null 2>&1 || fail 'start failed; installed files retained'
  adb shell pm list packages org.portos.echoscreen >"$temp/packages" 2>/dev/null || screen_fail
  if ! awk '$0 == "package:org.portos.echoscreen" { found=1 } END { exit !found }' "$temp/packages"; then
    adb install -r "$bundle/apk/echo-screen.apk" >/dev/null 2>&1 || screen_fail
    adb shell "printf '%s\\n' org.portos.echoscreen > /data/local/hassmic/state/screen_home && chown system:system /data/local/hassmic/state/screen_home && chmod 644 /data/local/hassmic/state/screen_home" >/dev/null 2>&1 || screen_fail
    adb shell am start -n org.portos.echoscreen/.MainActivity >/dev/null 2>&1 || screen_fail
  fi
  printf '%s\n' 'Echo installed and started' 'Next: ChatGPT sign-in on the Echo screen; then Port OS link.'
}
fail() {
  printf 'echo-release: %s\n' "$1" >&2
  exit 1
}
usage() {
  printf '%s\n' 'echo-release: invalid usage; see --help (source installer requires --version)' >&2
  exit 2
}
screen_fail() {
  fail 'screen setup failed; bridge already started'
}
download() {
  if [ "$downloader" = curl ]; then
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
      --connect-timeout 20 --max-time 600 --output "$2" "$1" >/dev/null 2>&1 || fail 'download failed'
  else
    wget --https-only --timeout=60 --tries=1 --output-document="$2" "$1" >/dev/null 2>&1 || fail 'download failed'
  fi
}
main "$@" </dev/null
