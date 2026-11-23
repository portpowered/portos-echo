#!/bin/sh
# Paired release entry point. Parse the entire script before reading piped input.
main() {
  set -eu
  umask 077
  version='@ECHO_VERSION@'
  config=''
  api_key=''
  seen=' '
  while [ "$#" -gt 0 ]; do
    flag=$1
    case $seen in *" $flag "*) usage ;; esac
    seen="$seen$flag "
    case $flag in
      --help)
        [ "$#" = 1 ] || usage
        printf '%s\n' 'Usage: sh install.sh [--version VERSION] [--config FILE] [--openai-api-key-file FILE]' \
          'Linux x86_64; requires curl or wget, sha256sum and ordinary coreutils.' \
          'The source bootstrap requires --version. echo-setup locates adb.'
        exit 0
        ;;
      --version | --config | --openai-api-key-file)
        [ "$#" -ge 2 ] || usage
        case $2 in '' | --*) usage ;; esac
        case $flag in
          --version) version=${2#v} ;;
          --config) config=$2 ;;
          --openai-api-key-file) api_key=$2 ;;
        esac
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
  if [ -n "$api_key" ]; then
    [ -f "$api_key" ] && [ -r "$api_key" ] || fail 'API key file is not readable'
  fi
  for tool in sha256sum mktemp awk chmod rm; do
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
  asset="echo-setup-$version-linux-x86_64"
  binary=$temp/$asset
  base="https://github.com/portpowered/portos-echo/releases/download/v$version"
  download "$base/$asset" "$binary"
  download "$base/SHA256SUMS" "$temp/SHA256SUMS"
  digest=$(awk -v name="$asset" '
    { sub(/\r$/, "") }
    $2 == name { count++; if (NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-f]/) bad=1; hash=$1 }
    END { if (count != 1 || bad) exit 1; print hash }' "$temp/SHA256SUMS") || fail 'invalid installer checksum'
  (cd "$temp" && printf '%s  %s\n' "$digest" "$asset" | sha256sum -c -) >/dev/null 2>&1 || fail 'installer checksum mismatch'
  chmod 700 "$binary" 2>/dev/null || fail 'installer permission change failed'
  set -- install --version "$version"
  [ -z "$config" ] || set -- "$@" --config "$config"
  # Only the path crosses argv; echo-setup reads the key itself.
  [ -z "$api_key" ] || set -- "$@" --openai-api-key-file "$api_key"
  "$binary" "$@"
}
fail() {
  printf 'echo-release: %s\n' "$1" >&2
  exit 1
}
usage() {
  printf '%s\n' 'echo-release: invalid usage; see --help (source installer requires --version)' >&2
  exit 2
}
download() {
  if [ "$downloader" = curl ]; then
    curl --disable --fail --silent --show-error --location --max-redirs 10 --proto '=https' --proto-redir '=https' \
      --connect-timeout 20 --max-time 600 --output "$2" "$1" >/dev/null 2>&1 || fail 'download failed'
  else
    wget_download "$1" "$2"
  fi
}
wget_download() {
  url=$1
  redirects=0
  while :; do
    case $url in https://?*) ;; *) fail 'download failed' ;; esac
    # --https-only covers recursive links, not redirects. Never let wget follow.
    if wget --no-config --https-only --max-redirect=0 --server-response --timeout=60 --tries=1 \
      --output-document="$2" "$url" >/dev/null 2>"$temp/headers"; then
      return
    fi
    location=$(awk '
      $1 ~ /^HTTP\// { status=$2; location="" }
      /^[ \t]+/ && tolower($1) == "location:" { sub(/^[^:]*:[ \t]*/, ""); sub(/\r$/, ""); location=$0 }
      END { if (status !~ /^(301|302|303|307|308)$/ || location == "" || location ~ /[[:space:]]/) exit 1; print location }
      ' "$temp/headers") || fail 'download failed'
    [ "$redirects" -lt 10 ] || fail 'download failed'
    redirects=$((redirects + 1))
    authority=${url#https://}
    authority=${authority%%/*}
    case $location in
      https://*) url=$location ;;
      //*) url=https:$location ;;
      /*) url=https://$authority$location ;;
      \?*) url=${url%%\?*}$location ;;
      *)
        case ${location%%/*} in *:*) fail 'download failed' ;; esac
        url=${url%/*}/$location
        ;;
    esac
  done
}
main "$@" </dev/null
