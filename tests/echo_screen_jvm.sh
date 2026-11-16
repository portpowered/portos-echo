#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
if [ -n "${JAVA_HOME_17_X64:-}" ]; then
  PATH="$JAVA_HOME_17_X64/bin:$PATH"
  export PATH
fi
out="$root/target/echo-screen/host-tests"
mkdir -p "$out"
src="$root/apps/echo-screen/src/org/portos/echoscreen"
javac --release 8 -d "$out" "$src/Secret.java" "$src/RetryPolicy.java" \
  "$src/ScreenConfig.java" "$src/ScreenLoad.java" "$root/tests/echo-screen/ScreenTest.java"
java -cp "$out" org.portos.echoscreen.ScreenTest
