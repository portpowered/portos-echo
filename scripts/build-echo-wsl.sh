#!/bin/sh
# Windows entry adapter only; compilation and inspection stay in build-echo.sh.
set -eu
fail() {
  printf 'build-echo: %s\n' "$1" >&2
  exit 1
}
command -v wsl >/dev/null || fail 'installed WSL Ubuntu required; no install attempted'
export MSYS_NO_PATHCONV=1
guest_path() {
  case $1 in
    [A-Za-z]:* | //?* | \\*)
      wsl -d Ubuntu --exec wslpath -u "$1" || fail 'WSL wslpath could not translate selected path'
      ;;
    /*) printf '%s\n' "$1" ;;
    *) fail 'select an absolute guest, Windows drive or UNC path' ;;
  esac
}
command -v cygpath >/dev/null || fail 'installed Git for Windows cygpath required'
root=$(guest_path "$(cygpath -am .)")
ndk=${ANDROID_NDK_HOME:-${ANDROID_NDK_LATEST_HOME:-}}
if [ -n "$ndk" ]; then
  ndk=$(guest_path "$ndk")
fi
target=${CARGO_TARGET_DIR:-}
if [ -n "$target" ]; then
  case $target in
    [A-Za-z]:* | /* | \\*) target=$(guest_path "$target") ;;
    *) ;; # Relative Cargo target directories remain relative to the checkout.
  esac
fi
# Constant shell program: caller paths and packages are positional data only.
# Guest login supplies its provisioned Rust PATH; Windows Rust homes never win.
guest=$(
  cat <<'GUEST'
  set -eu
  cd -- "$1"
  export ANDROID_NDK_HOME=${2:-$HOME/android/android-ndk-r27d}
  unset ANDROID_NDK_LATEST_HOME CARGO_HOME RUSTUP_HOME RUSTUP_TOOLCHAIN RUSTC RUSTC_WRAPPER
  if [ -n "$3" ]; then export CARGO_TARGET_DIR=$3; else unset CARGO_TARGET_DIR; fi
  export RUSTUP_AUTO_INSTALL=0
  shift 3
  exec sh scripts/build-echo.sh "$@"
GUEST
)
wsl -d Ubuntu --exec sh -lc "$guest" sh "$root" "$ndk" "$target" "$@" || fail 'WSL Ubuntu build failed; check guest provisioning and diagnostic above (no install attempted)'
