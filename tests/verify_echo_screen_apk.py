"""T2 contract witness: inspect an already-built APK using the actual SDK tools."""

import os
from pathlib import Path
import re
import subprocess
import sys


def element_blocks(tree, tag):
    return [body for _, body in
            re.findall(rf"(?m)^( +)E: {tag}(?=\s|$)[^\n]*\n((?:\1 +[^\n]*\n)*)", tree)]


def verify_activity(manifest):
    manifest = manifest.replace("A: http://schemas.android.com/apk/res/android:", "A: android:")
    activities = [body for body in element_blocks(manifest, "activity")
                  if re.search(r'android:name[^=]*="(?:org\.portos\.echoscreen)?\.MainActivity"', body)]
    if len(activities) != 1:
        raise RuntimeError("APK is missing a unique MainActivity")
    activity = activities[0]
    if not re.search(r'android:permission[^=]*="android\.permission\.DUMP"', activity):
        raise RuntimeError("APK launcher caller permission differs")
    for attribute in ("directBootAware", "exported"):
        if not re.search(rf'(?m)^ +A: android:{attribute}[^=]*=(?:true|\(type 0x12\)0xffffffff)$', activity):
            raise RuntimeError(f"APK MainActivity requires {attribute}=true")
    filters = []
    for body in element_blocks(activity, "intent-filter"):
        actions = element_blocks(body, "action")
        if len(actions) != 1 or not re.search(r'android:name[^=]*="android\.intent\.action\.MAIN"', actions[0]):
            raise RuntimeError("APK MainActivity intent filter requires MAIN")
        categories = re.findall(r'android:name[^=]*="android\.intent\.category\.([A-Z_]+)"', body)
        filters.append(sorted(categories))
    if sorted(filters) != [["DEFAULT", "HOME"], ["LAUNCHER"]]:
        raise RuntimeError("APK MainActivity requires distinct HOME/DEFAULT and LAUNCHER filters")


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
        command = [str(tools / tool)]
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
        r"^uses-permission: name='android\.permission\.CAMERA'$",
    )
    if not all(re.search(pattern, badging, re.MULTILINE) for pattern in required):
        raise RuntimeError("APK metadata does not match the echo-screen contract")
    manifest = run("aapt2", "dump", "xmltree", "--file", "AndroidManifest.xml")
    verify_activity(manifest)
    run("zipalign", "-c", "4")
    run("apksigner", "verify", "--verbose")
    print("echo-screen APK: package=org.portos.echoscreen minSdk=28 targetSdk=28 "
          "INTERNET=true CAMERA=true MainActivity=DUMP directBootAware=true "
          "MAIN/HOME/DEFAULT=true MAIN/LAUNCHER=true signature=verified")


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
