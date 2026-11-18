#!/bin/sh
# Coordinator-only on hardware; tests put the prebuilt fake adb on PATH.
set -eu

usage() {
  printf '%s\n' 'Usage: sh scripts/echo-screen-install.sh [--url <page-url>]' \
    '       sh scripts/echo-screen-install.sh --uninstall' >&2
  exit 2
}
fail() {
  printf 'echo-screen: %s\n' "$1" >&2
  exit 1
}

url=
uninstall=false
case $# in
  0) ;;
  1)
    [ "$1" = --uninstall ] || usage
    uninstall=true
    ;;
  2)
    [ "$1" = --url ] || usage
    url=$2
    ;;
  *) usage ;;
esac
if [ "$#" = 2 ]; then
  case $url in
    http://* | https://*) ;;
    *) usage ;;
  esac
  case $url in
    *[[:space:]]*) usage ;;
  esac
  authority=${url#*://}
  authority=${authority%%[/?#]*}
  case $authority in
    '' | *@*) usage ;;
  esac
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
apk="$root/target/echo-screen/echo-screen.apk"
if [ "$uninstall" = false ]; then
  [ -f "$apk" ] && [ -r "$apk" ] || fail 'APK is not readable'
fi
command -v adb >/dev/null 2>&1 || fail 'adb is required'
umask 077
log=$(mktemp) || fail 'private log creation failed'
trap 'rm -f "$log"' 0
trap 'exit 1' HUP INT TERM
run_adb() {
  message=$1
  shift
  adb "$@" >"$log" 2>&1 || fail "$message"
}
if [ "$uninstall" = true ]; then
  run_adb 'home removal failed' shell rm -f /data/local/hassmic/state/screen_home
  run_adb 'uninstall failed' uninstall org.portos.echoscreen
  printf '%s\n' 'echo-screen uninstalled'
else
  run_adb 'install failed' install -r "$apk"
  run_adb 'home setup failed' shell "printf '%s\\n' org.portos.echoscreen > /data/local/hassmic/state/screen_home && chown system:system /data/local/hassmic/state/screen_home && chmod 644 /data/local/hassmic/state/screen_home"
  if [ -n "$url" ]; then
    # adb joins shell argv with spaces: protect the URL for the remote shell too.
    quoted=$(printf '%s' "$url" | sed "s/'/'\\\\''/g")
    run_adb 'launch failed' shell am start -n org.portos.echoscreen/.MainActivity --es url "'$quoted'"
  else
    run_adb 'launch failed' shell am start -n org.portos.echoscreen/.MainActivity
  fi
  printf '%s\n' 'echo-screen installed'
fi
