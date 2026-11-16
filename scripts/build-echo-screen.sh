#!/bin/sh
set -eu
umask 077
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fail() {
  printf 'echo-screen: %s\n' "$1" >&2
  exit 1
}
[ -n "${ANDROID_HOME:-}" ] || fail 'ANDROID_HOME is required (no SDK packages are installed)'
tools="$ANDROID_HOME/build-tools/35.0.0"
android_jar="$ANDROID_HOME/platforms/android-34/android.jar"
[ -r "$android_jar" ] || fail 'platform android-34 is required'
for tool in aapt2 zipalign; do
  [ -x "$tools/$tool" ] || fail "build-tools 35.0.0 $tool is required"
done
# The Windows SDK ships batch launchers for these same SDK Java tools.
# Invoke their jars directly from POSIX sh rather than crossing into cmd.exe.
d8() {
  if [ -f "$tools/d8" ]; then
    "$tools/d8" "$@"
  else
    [ -r "$tools/lib/d8.jar" ] || fail 'build-tools 35.0.0 d8 is required'
    java -cp "$tools/lib/d8.jar" com.android.tools.r8.D8 "$@"
  fi
}
apksigner() {
  if [ -f "$tools/apksigner" ]; then
    "$tools/apksigner" "$@"
  else
    [ -r "$tools/lib/apksigner.jar" ] || fail 'build-tools 35.0.0 apksigner is required'
    java -jar "$tools/lib/apksigner.jar" "$@"
  fi
}
if [ -n "${JAVA_HOME_17_X64:-}" ]; then
  JAVA_HOME=$JAVA_HOME_17_X64
  PATH="$JAVA_HOME/bin:$PATH"
  export JAVA_HOME PATH
fi
for tool in java javac jar keytool; do
  command -v "$tool" >/dev/null 2>&1 || fail "JDK $tool is required"
done

build="$root/target/echo-screen"
mkdir -p "$build"
work=$(mktemp -d "$build/build.XXXXXXXX")
mkdir -p "$work/classes" "$work/dex"
# Tool output stays private: operator keystore paths/diagnostics are not public logs.
run() {
  stage=$1
  shift
  "$@" >"$work/tool.log" 2>&1 || fail "$stage failed (private build log under target/echo-screen)"
}
src="$root/apps/echo-screen/src/org/portos/echoscreen"
run 'manifest link' "$tools/aapt2" link -o "$work/base.apk" -I "$android_jar" \
  --manifest "$root/apps/echo-screen/AndroidManifest.xml" --min-sdk-version 28 --target-sdk-version 28
run 'Java compilation' javac --release 8 -classpath "$android_jar" -d "$work/classes" "$src"/*.java
run 'class archive' jar cf "$work/classes.jar" -C "$work/classes" .
run 'dex compilation' d8 --min-api 28 --lib "$android_jar" --output "$work/dex" "$work/classes.jar"
run 'dex packaging' jar uf "$work/base.apk" -C "$work/dex" classes.dex
run 'zip alignment' "$tools/zipalign" -f 4 "$work/base.apk" "$work/aligned.apk"

if [ -n "${ECHO_SCREEN_KEYSTORE:-}" ]; then
  [ -f "$ECHO_SCREEN_KEYSTORE" ] && [ -r "$ECHO_SCREEN_KEYSTORE" ] || fail 'operator keystore is not readable'
  # apksigner prompts on the terminal; passwords are never passed in argv.
  apksigner sign --ks "$ECHO_SCREEN_KEYSTORE" \
    --out "$work/signed.apk" "$work/aligned.apk" || fail 'operator signing failed'
else
  keystore="$build/build-only.keystore"
  if [ ! -f "$keystore" ]; then
    # Public test-key password, not an operator credential. Key material is local.
    run 'build key generation' keytool -genkeypair -keystore "$work/build-only.keystore" \
      -storepass android -keypass android -alias echo-screen -dname 'CN=Echo Screen Build Only' \
      -keyalg RSA -keysize 2048 -validity 3650
    mv "$work/build-only.keystore" "$keystore"
  fi
  run 'build signing' apksigner sign --ks "$keystore" --ks-pass pass:android \
    --out "$work/signed.apk" "$work/aligned.apk"
fi
run 'signature verification' apksigner verify "$work/signed.apk"
run 'alignment verification' "$tools/zipalign" -c 4 "$work/signed.apk"
# Only publish a fully signed/verified replacement. Failed builds leave the old APK.
mv "$work/signed.apk" "$build/echo-screen.apk"
printf '%s\n' 'echo-screen APK built'
