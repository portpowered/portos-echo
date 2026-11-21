#!/bin/sh
# Sourceable helpers; the entry point is guarded for focused unit checks.
set -eu

codex_fail() {
  printf 'build-codex-armv7: %s\n' "$1" >&2
  exit 1
}

codex_path() {
  case $1 in
    /*) ;;
    *) codex_fail 'work/output paths must be absolute' ;;
  esac
  realpath -m -- "$1"
}

codex_empty() {
  [ ! -e "$1" ] || { [ -d "$1" ] && [ -z "$(find "$1" -mindepth 1 -maxdepth 1 -print)" ]; } ||
    codex_fail 'work/output directory must be new or empty'
}

codex_paths() {
  work=$(codex_path "${CODEX_ARMV7_WORK_DIR:?set CODEX_ARMV7_WORK_DIR}")
  output=$(codex_path "${CODEX_ARMV7_OUTPUT_DIR:?set CODEX_ARMV7_OUTPUT_DIR}")
  for candidate in "$work" "$output"; do
    case $candidate/ in
      "$repo/"* | /) codex_fail 'work/output must be outside the repository' ;;
    esac
    case $repo/ in
      "$candidate/"*) codex_fail 'work/output cannot contain the repository' ;;
    esac
    codex_empty "$candidate"
    [ -d "$(dirname -- "$candidate")" ] || codex_fail 'work/output parent must exist'
  done
  case $work/ in "$output/"*) codex_fail 'work/output overlap' ;; esac
  case $output/ in "$work/"*) codex_fail 'work/output overlap' ;; esac
}

codex_prerequisites() {
  for tool in git curl cargo rustc realpath find sha256sum file patch cmake make perl python3 tar; do
    command -v "$tool" >/dev/null || codex_fail "missing prerequisite: $tool"
  done
  ndk=${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}
  tools="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
  for tool in armv7a-linux-androideabi24-clang armv7a-linux-androideabi24-clang++ llvm-ar llvm-strip llvm-readelf; do
    [ -x "$tools/$tool" ] || codex_fail "missing NDK tool: $tool"
  done
  grep -Eq '^Pkg.Revision[[:space:]]*=[[:space:]]*27\.3\.13750724[[:space:]]*$' "$ndk/source.properties" ||
    codex_fail 'NDK 27.3.13750724 required'
  [ -r "$ndk/build/cmake/android.toolchain.cmake" ] || codex_fail 'missing NDK CMake toolchain'
  compiler=$(rustc +1.100.0 --version)
  "$tools/armv7a-linux-androideabi24-clang" --version >&2
}

codex_environment() {
  export PATH="$tools:$PATH" ANDROID_NDK_ROOT="$ndk"
  export CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR="$work/target"
  export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$tools/armv7a-linux-androideabi24-clang"
  export CC_armv7_linux_androideabi="$tools/armv7a-linux-androideabi24-clang"
  export CXX_armv7_linux_androideabi="$tools/armv7a-linux-androideabi24-clang++"
  export AR_armv7_linux_androideabi="$tools/llvm-ar"
  export ANDROID_PLATFORM=android-24
  export CMAKE_TOOLCHAIN_FILE="$ndk/build/cmake/android.toolchain.cmake"
  export RUSTUP_AUTO_INSTALL=0
  # Encode each argument separately so external paths containing spaces survive.
  CARGO_ENCODED_RUSTFLAGS=$(
    python3 - "$work" <<'PY'
import os
from pathlib import Path
import sys
separator = "\x1f"
encoded = os.environ.get("CARGO_ENCODED_RUSTFLAGS")
flags = os.environ.get("RUSTFLAGS", "").split() if encoded is None else encoded.split(separator)
flags = [flag for flag in flags if flag]
cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
flags += ["--remap-path-prefix=" + str(cargo_home) + "=/codex-armv7/cargo",
          "--remap-path-prefix=" + sys.argv[1] + "=/codex-armv7/work"]
print(separator.join(flags), end="")
PY
  )
  export CARGO_ENCODED_RUSTFLAGS
}

codex_release() {
  # No release is pinned: the newest stable one, or the one CODEX_RELEASE names.
  release=$(python3 "$repo/scripts/codex_release.py" resolve "$repo") || codex_fail 'cannot select a Codex release'
  tag=$(printf '%s\n' "$release" | sed -n 's/^tag=//p')
  commit=$(printf '%s\n' "$release" | sed -n 's/^commit=//p')
  [ -n "$tag" ] && [ -n "$commit" ] || codex_fail 'cannot select a Codex release'
  printf 'build-codex-armv7: building %s at %s\n' "$tag" "$commit" >&2
}

codex_source() {
  git init -q "$work/source"
  git -C "$work/source" fetch --depth 1 https://github.com/openai/codex.git "$commit"
  actual=$(git -C "$work/source" rev-parse FETCH_HEAD)
  [ "$actual" = "$commit" ] || codex_fail 'wrong source identity'
  git -C "$work/source" checkout -q --detach FETCH_HEAD
  # Content-anchored steps for any release: lock versions, Android OpenSSL, mode width.
  python3 "$repo/third_party/codex/prepare_source.py" "$work/source" ||
    codex_fail 'source preparation failed'
}

codex_check_lock() {
  python3 - "$work/source/codex-rs" <<'PY'
from pathlib import Path
import sys
import tomllib

root = Path(sys.argv[1])
with (root / "Cargo.toml").open("rb") as manifest:
    expected = tomllib.load(manifest)["workspace"]["package"]["version"]
with (root / "Cargo.lock").open("rb") as lock:
    packages = tomllib.load(lock)["package"]
for package in packages:
    if "source" not in package and package["version"] != expected:
        raise SystemExit(
            f"build-codex-armv7: local package {package['name']} is locked at "
            f"{package['version']}; expected {expected}"
        )
PY
}

codex_pagable() {
  # The coordinator authorized only this locked crate's ARM layout assertion.
  checksum=$(
    python3 - "$work/source/codex-rs/Cargo.lock" <<'PY'
import sys
import tomllib
with open(sys.argv[1], "rb") as source:
    packages = tomllib.load(source)["package"]
package, = (p for p in packages if p["name"] == "pagable")
if (package["version"], package["source"]) != ("0.4.1", "registry+https://github.com/rust-lang/crates.io-index"):
    raise SystemExit("build-codex-armv7: pagable step is stale: the release locks pagable " + package["version"])
print(package["checksum"])
PY
  )
  curl --fail --location --proto '=https' --tlsv1.2 \
    https://static.crates.io/crates/pagable/pagable-0.4.1.crate -o "$work/pagable.crate"
  printf '%s  %s\n' "$checksum" "$work/pagable.crate" | sha256sum -c - >&2
  mkdir "$work/vendor"
  tar -xzf "$work/pagable.crate" -C "$work/vendor"
  patch -d "$work/vendor/pagable-0.4.1" -p1 <"$repo/third_party/codex/patches/0003-pagable-arm-layout.patch"
  python3 - "$work" <<'PY'
import json
import sys
from pathlib import Path
work = Path(sys.argv[1])
(work / "cargo-config.toml").write_text(
    "paths = [" + json.dumps(str(work / "vendor/pagable-0.4.1")) + "]\n")
PY
}

codex_mxc() {
  # Codex locks Microsoft's sandbox SDK from git, and its engine names a module
  # it compiles only for Windows, Linux and macOS. The locked checkout is
  # vendored with those calls configured out on other hosts.
  (cd "$work/source/codex-rs" && cargo +1.100.0 metadata --locked --format-version 1 \
    --filter-platform armv7-linux-androideabi) >"$work/metadata.json" ||
    codex_fail 'cannot read the locked dependency graph'
  python3 "$repo/third_party/codex/prepare_source.py" --mxc "$work" || codex_fail 'mxc preparation failed'
}

codex_ca() {
  curl --fail --location --proto '=https' --tlsv1.2 \
    https://curl.se/ca/cacert-2026-11-05.pem -o "$stage/cacert.pem"
  printf '%s  %s\n' '427c064216761a1e72a8b30f0d954d08cb6e4435a82091fb865afd20c8497285' \
    "$stage/cacert.pem" | sha256sum -c - >&2
}

codex_build() {
  (cd "$work/source/codex-rs" && cargo +1.100.0 --config "$work/cargo-config.toml" build --locked --release \
    --target armv7-linux-androideabi -p codex-app-server --bin codex-app-server) >&2
  cp "$work/target/armv7-linux-androideabi/release/codex-app-server" "$stage/codex-app-server"
  "$tools/llvm-strip" "$stage/codex-app-server"
}

codex_inspect() {
  description=$(file "$stage/codex-app-server")
  printf '%s\n' "$description" >&2
  printf '%s\n' "$description" | grep -Eq 'ELF 32-bit.*ARM.*EABI5.*interpreter /system/bin/linker' ||
    codex_fail 'expected Android ARM ELF32 EABI5 executable'
  headers=$("$tools/llvm-readelf" -h -l -d "$stage/codex-app-server")
  printf '%s\n' "$headers" >&2
  printf '%s\n' "$headers" | grep -Eq 'Class:[[:space:]]+ELF32' || codex_fail 'expected ELF32'
  printf '%s\n' "$headers" | grep -Eq 'Machine:[[:space:]]+ARM[[:space:]]*$' || codex_fail 'expected ARM'
  printf '%s\n' "$headers" | grep -Eq 'Flags:[[:space:]]+0x0*5[[:xdigit:]]{6}([[:space:],]|$)' || codex_fail 'expected EABI5'
  printf '%s\n' "$headers" | grep -Fq '[Requesting program interpreter: /system/bin/linker]' || codex_fail 'expected Android linker'
  needed=$(printf '%s\n' "$headers" | sed -n 's/.*(NEEDED).*\[\([^]]*\)\].*/\1/p')
  [ -n "$needed" ] || codex_fail 'missing Android dependencies'
  for soname in $needed; do
    case $soname in
      libc.so | libm.so | libdl.so | liblog.so | libandroid.so) ;;
      *) codex_fail "non-system dependency: $soname" ;;
    esac
  done
  needed=$(printf '%s\n' "$needed" | paste -sd, -)
  size=$(wc -c <"$stage/codex-app-server" | tr -d '[:space:]')
  digest=$(sha256sum "$stage/codex-app-server")
  digest=${digest%% *}
}

codex_cleanup() {
  # Only invocation-owned staging files and empty lock directories are removed.
  if [ -n "${stage:-}" ]; then
    rm -f -- "$stage/codex-app-server" "$stage/cacert.pem" "$stage/LICENSE" "$stage/NOTICE"
    rmdir -- "$stage"
  fi
  if [ "${output_claimed:-false}" = true ]; then rmdir -- "$output.lock"; fi
  if [ "${work_claimed:-false}" = true ]; then rmdir -- "$work.lock"; fi
}

codex_main() {
  stage='' work_claimed=false output_claimed=false
  repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
  codex_prerequisites
  codex_release
  codex_paths
  trap codex_cleanup EXIT
  trap 'exit 1' HUP INT TERM
  mkdir -- "$work.lock" || codex_fail 'work already claimed'
  work_claimed=true
  mkdir -- "$output.lock" || codex_fail 'output already claimed'
  output_claimed=true
  codex_empty "$work"
  codex_empty "$output"
  mkdir -p -- "$work"
  stage=$(mktemp -d "$output.staging.XXXXXX")
  codex_environment
  codex_source >&2
  codex_check_lock
  codex_pagable >&2
  codex_mxc >&2
  codex_ca
  codex_build
  codex_inspect
  cp "$work/source/LICENSE" "$stage/LICENSE"
  cp "$work/source/NOTICE" "$stage/NOTICE"
  if [ -d "$output" ]; then rmdir -- "$output"; fi
  mv -T -- "$stage" "$output"
  stage=''
  printf 'release=%s\nsource_commit=%s\nrustc=%s\nndk=%s\nbinary=%s/codex-app-server\nca_bundle=%s/cacert.pem\nneeded=%s\nsize_bytes=%s\nsha256=%s\n' \
    "$tag" "$commit" "$compiler" '27.3.13750724/API24' "$output" "$output" "$needed" "$size" "$digest"
}

if [ "${CODEX_ARMV7_SOURCE_ONLY:-0}" != 1 ]; then codex_main; fi
