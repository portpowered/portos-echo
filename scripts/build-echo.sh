#!/bin/sh
# Build only with installed tools/cached dependencies. Provisioning belongs to CI setup.
set -eu
binary="${CARGO_TARGET_DIR:-target}/armv7-linux-androideabi/release/echo-bridge"
has_binary=false
for package; do
  if [ "$package" = echo-bridge ]; then
    has_binary=true
  fi
done
# Clean before provisioning probes as well as after any failed build/inspection.
cleanup() {
  status=$?
  if [ "$status" -ne 0 ] && "$has_binary"; then
    rm -f -- "$binary"
  fi
}
trap cleanup 0
if "$has_binary"; then
  rm -f -- "$binary"
fi
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    sh "$(dirname -- "$0")/build-echo-wsl.sh" "$@"
    exit $?
    ;;
esac
fail() {
  printf 'build-echo: %s\n' "$1" >&2
  exit 1
}
ndk=${ANDROID_NDK_HOME:-${ANDROID_NDK_LATEST_HOME:-}}
[ -n "$ndk" ] || fail 'set ANDROID_NDK_HOME (or ANDROID_NDK_LATEST_HOME) to an installed Android NDK; no download attempted'
[ -d "$ndk" ] || fail 'selected Android NDK directory does not exist'
ANDROID_NDK_HOME=$(CDPATH='' cd -- "$ndk" && pwd)
tools="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
cc="$tools/armv7a-linux-androideabi24-clang"
cxx="$tools/armv7a-linux-androideabi24-clang++"
ar="$tools/llvm-ar"
for tool in "$cc" "$cxx" "$ar" "$tools/llvm-readelf"; do
  [ -x "$tool" ] || fail 'selected Android NDK needs Linux x86_64 API-24 clang, clang++, llvm-ar and llvm-readelf'
done
[ -r "$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" ] || fail 'selected Android NDK lacks its CMake Android toolchain'
[ -r "$ANDROID_NDK_HOME/source.properties" ] || fail 'selected Android NDK lacks source.properties'
[ "$#" -gt 0 ] || fail 'no packages selected'
export ANDROID_NDK_HOME
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$cc"
export CC_armv7_linux_androideabi="$cc"
export CXX_armv7_linux_androideabi="$cxx"
export AR_armv7_linux_androideabi="$ar"
# opusic-sys's existing bundled build reads these Android CMake definitions.
export ANDROID_PLATFORM=android-24
export RUSTUP_AUTO_INSTALL=0
cat "$ANDROID_NDK_HOME/source.properties"
"$cc" --version || fail 'selected API-24 compiler failed; check installed NDK'
rustc --version || fail 'installed pinned Rust toolchain unavailable; no install attempted'

# The for-list is captured before modifying argv; preserve each package literally.
for package; do
  set -- "$@" -p "$package"
  shift
done
cargo build --offline --locked --release --target armv7-linux-androideabi "$@" || fail 'offline locked Cargo build failed; check installed target/cache and compiler diagnostic above'
for argument; do
  if [ "$argument" != -p ]; then
    printf 'build-echo: built %s for armv7-linux-androideabi (release)\n' "$argument"
  fi
done
if "$has_binary"; then
  [ -f "$binary" ] || fail 'expected echo-bridge release binary is missing'
  file "$binary"
  headers=$("$tools/llvm-readelf" -h -l -d "$binary") || fail 'NDK ELF inspection failed'
  printf '%s\n' "$headers"
  printf '%s\n' "$headers" | grep -Eq 'Class:[[:space:]]+ELF32' || fail 'expected ELF32 class'
  printf '%s\n' "$headers" | grep -Eq 'Machine:[[:space:]]+ARM[[:space:]]*$' || fail 'expected ARM machine'
  # EF_ARM_EABIMASK is 0xff000000; LLVM prints numeric flags without GNU's label.
  printf '%s\n' "$headers" | grep -Eq '^[[:space:]]*Flags:[[:space:]]+0x0*5[[:xdigit:]]{6}([[:space:],]|$)' || fail 'expected Version5 EABI'
  printf '%s\n' "$headers" | grep -Fq '[Requesting program interpreter: /system/bin/linker]' || fail 'expected Android /system/bin/linker interpreter'
  if printf '%s\n' "$headers" | grep -E 'NEEDED.*lib(ssl|crypto)' >/dev/null; then
    fail 'unexpected OpenSSL dynamic dependency'
  fi
  bytes=$(wc -c <"$binary" | tr -d '[:space:]')
  printf 'build-echo: binary size %s bytes\n' "$bytes"
else
  printf '%s\n' 'build-echo: library-only build; executable/interpreter/size/upload unproved (W4-BINARY)'
fi
