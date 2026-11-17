#!/bin/sh
# Windows adapters source this file; Linux never resolves the setting.
echo_wsl_distro() {
  echo_wsl_selected=${ECHO_WSL_DISTRO:-'Ubuntu'}
  case $echo_wsl_selected in
    '' | *[!A-Za-z0-9._-]*)
      printf '%s\n' 'ECHO_WSL_DISTRO must contain only A-Za-z0-9._-' >&2
      return 1
      ;;
  esac
  printf '%s\n' "$echo_wsl_selected"
}
echo_wsl_failure() {
  printf "WSL distro '%s': operation failed; check guest provisioning\n" "$1" >&2
}
echo_wsl_run() {
  echo_wsl_selected=$1
  shift
  MSYS_NO_PATHCONV=1 wsl -d "$echo_wsl_selected" --exec "$@" || {
    echo_wsl_status=$?
    echo_wsl_failure "$echo_wsl_selected"
    return "$echo_wsl_status"
  }
}
# Retain stdin and collect the launcher, including when the caller is cancelled.
echo_wsl_exec() {
  echo_wsl_selected=$1
  shift
  MSYS_NO_PATHCONV=1 wsl -d "$echo_wsl_selected" --exec "$@" <&0 &
  echo_wsl_pid=$!
  trap 'kill -TERM "$echo_wsl_pid" 2>/dev/null || :; wait "$echo_wsl_pid" || :; exit 1' HUP INT TERM
  echo_wsl_status=0
  wait "$echo_wsl_pid" || echo_wsl_status=$?
  trap - HUP INT TERM
  if [ "$echo_wsl_status" -ne 0 ]; then echo_wsl_failure "$echo_wsl_selected"; fi
  exit "$echo_wsl_status"
}
# Native Python owns every advertised listener; WSL sees only guest endpoints.
echo_wsl_relay() {
  echo_wsl_relay_root=$1
  echo_wsl_selected=$2
  echo_wsl_controller=$3
  shift 3
  MSYS_NO_PATHCONV=1 python "$(cygpath -am "$echo_wsl_relay_root/scripts/wsl_relay.py")" \
    --distro "$echo_wsl_selected" --controller "$echo_wsl_controller" "$@" <&0 &
  echo_wsl_pid=$!
  trap 'kill -TERM "$echo_wsl_pid" 2>/dev/null || :; wait "$echo_wsl_pid" || :; exit 1' HUP INT TERM
  echo_wsl_status=0
  wait "$echo_wsl_pid" || echo_wsl_status=$?
  trap - HUP INT TERM
  if [ "$echo_wsl_status" -ne 0 ]; then echo_wsl_failure "$echo_wsl_selected"; fi
  exit "$echo_wsl_status"
}
