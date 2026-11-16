"""T2 contract witness: inspect an already-built APK using the actual SDK tools."""

import os
from pathlib import Path
import re
import subprocess
import sys


def main():
    sdk = os.environ.get("ANDROID_HOME")
    if not sdk:
        raise RuntimeError("ANDROID_HOME is required")
    tools = Path(sdk) / "build-tools" / "35.0.0"
    apk = Path(__file__).resolve().parents[1] / "target" / "echo-screen" / "echo-screen.apk"
    env = dict(os.environ)
    if env.get("JAVA_HOME_17_X64"):
        env["JAVA_HOME"] = env["JAVA_HOME_17_X64"]
        env["PATH"] = str(Path(env["JAVA_HOME"]) / "bin") + os.pathsep + env["PATH"]

    def run(tool, *args):
        if os.name == "nt" and tool == "apksigner":
            command = ["java", "-jar", str(tools / "lib" / "apksigner.jar")]
        else:
            command = [str(tools / (tool + ".exe" if os.name == "nt" else tool))]
        result = subprocess.run([*command, *args, str(apk)],
                                check=True, capture_output=True, text=True, env=env)
        print(result.stdout, end="")
        return result.stdout

    badging = run("aapt2", "dump", "badging")
    required = (
        r"^package: name='org\.portos\.echoscreen' ",
        r"^(?:minSdkVersion|sdkVersion):'28'$",
        r"^targetSdkVersion:'28'$",
        r"^uses-permission: name='android\.permission\.INTERNET'$",
    )
    if not all(re.search(pattern, badging, re.MULTILINE) for pattern in required):
        raise RuntimeError("APK metadata does not match the echo-screen contract")
    run("zipalign", "-c", "4")
    run("apksigner", "verify", "--verbose")
    print("echo-screen APK: package=org.portos.echoscreen minSdk=28 targetSdk=28 "
          "INTERNET=true signature=verified")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        # Never echo arbitrary tool diagnostics or operator paths.
        if isinstance(error, RuntimeError):
            print(f"echo-screen: {error}", file=sys.stderr)
        else:
            print("echo-screen: APK verification tool failed", file=sys.stderr)
        sys.exit(1)
