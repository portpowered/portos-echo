"""Build the embedded helper with the same installed tools as echo-screen."""

import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory


def main(out):
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
    if not sdk:
        raise SystemExit("Wi-Fi helper: set ANDROID_HOME to the installed Android SDK")
    sdk = Path(sdk)
    android = sdk / "platforms/android-34/android.jar"
    tools = sdk / "build-tools/35.0.0"
    d8 = [tools / "d8"]
    if not d8[0].is_file():
        d8 = ["java", "-cp", tools / "lib/d8.jar", "com.android.tools.r8.D8"]
    source = Path(__file__).with_name("EchoWifi.java")
    with TemporaryDirectory(prefix="echo-wifi-dex-") as work:
        subprocess.run(
            ["javac", "--release", "8", "-classpath", android, "-d", work, source],
            check=True,
        )
        subprocess.run(
            [*d8, "--min-api", "28", "--lib", android, "--output", out,
             *sorted(Path(work).glob("*.class"))],
            check=True,
        )


if __name__ == "__main__":
    main(Path(sys.argv[1]))
