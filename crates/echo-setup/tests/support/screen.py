"""T3 authored device shell; isolated Android tools/files, no adb or hardware."""
import json
import os
from pathlib import Path
import shlex
import sys

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "tests"))
from test_echo_install import Sandbox, require, SENTINEL, owner

PACKAGE_QUERY = ["pm", "list", "packages", "--show-versioncode", "org.portos.echoscreen"]
GRANT = ["pm", "grant", "org.portos.echoscreen", "android.permission.CAMERA"]
START = ["am", "start", "-n", "org.portos.echoscreen/.MainActivity"]


def android_tool():
    root = Path(os.environ["FAKE_ROOT"])
    args = sys.argv[2:]
    require((root / "remote/echo-bridge.lock").is_dir(), "screen mutation bypassed lock")
    with (root / "screen-ledger").open("a") as ledger:
        ledger.write(json.dumps(args) + "\n")
    print(SENTINEL, file=sys.stderr)
    fault = (root / "screen-fault").read_text() if (root / "screen-fault").exists() else ""
    if fault in (args[0], args[1] if args[0] == "pm" else "unused"):
        return 1
    version = root / "screen-version"
    if args == PACKAGE_QUERY:
        if version.exists():
            print("package:org.portos.echoscreen versionCode:" + version.read_text())
        return 0
    if args[:3] == ["pm", "install", "-r"]:
        require(Path(args[3]).read_bytes() == b"synthetic APK", "wrong APK installed")
        version.write_text("2")
        print("Success")
        return 0
    if args == GRANT:
        require(version.read_text() == "2", "grant preceded upgrade")
        (root / "screen-granted").touch()
        return 0
    require(args == START, "activity differs")
    require((root / "screen-granted").is_file(), "launch preceded grant")
    if fault == "launch-error":
        print("Error: " + SENTINEL, file=sys.stderr)
    print(SENTINEL)
    return 0


def fixture(box):
    # Supply the installed APK directly. Installation has its own witness and
    # does not need to run for each screen failure and retry.
    apk = box.device / "apk/echo-screen.apk"
    apk.parent.mkdir(parents=True)
    apk.write_bytes(b"synthetic APK")
    apk.chmod(0o644)
    state = box.root / "remote/hassmic/state"
    state.mkdir()
    for path in box.script.parent.glob("echo-install*.sh"):
        path.write_text(path.read_text().replace("/data/local", str(box.root / "remote")))
    command = shlex.quote(sys.executable) + " " + shlex.quote(str(Path(__file__).resolve()))
    for tool in ("pm", "am"):
        path = box.root / "remote-tools" / tool
        path.write_text("#!/bin/sh\nexec " + command + " --tool " + tool + ' "$@"\n')
        path.chmod(0o755)
    box.env["PATH"] = str(box.root / "remote-tools") + ":/usr/bin:/bin"
    return state


def run_cases():
    groups = {"lifecycle": ("missing", "older", "present", "locked"),
              "dependency": ("pm", "install", "grant", "am", "launch-error"),
              "paths": ("home-link", "stage-link", "hardlink", "parent-link")}
    group = sys.argv[1]
    for fault in groups[group]:
        with Sandbox() as box:
            state = fixture(box)
            home = state / "screen_home"
            outside = box.root / "untouched"
            outside.write_text("untouched")
            if fault in ("older", "present"):
                (box.root / "screen-version").write_text("1" if fault == "older" else "2")
                (box.root / "screen-app-data").write_text("preserved")
            if fault in groups["dependency"]:
                (box.root / "screen-fault").write_text(fault)
            if fault == "home-link":
                home.symlink_to(outside)
            if fault == "stage-link":
                (state / ".echo-screen-home").symlink_to(outside)
            if fault == "hardlink":
                os.link(outside, state / ".echo-screen-home")
            if fault == "parent-link":
                state.rename(state.with_name("saved"))
                state.symlink_to(state.with_name("saved"), target_is_directory=True)
            if fault == "locked":
                (box.root / "remote/echo-bridge.lock").mkdir()
            box.run(["screen-setup"], 2)
            okay = fault in ("missing", "older", "present")
            result = box.run(["--device", "screen-setup"], 0 if okay else 1)
            require(result.stdout == ("echo-install: screen ready\n" if okay else ""), "screen result differs")
            require(outside.read_text() == "untouched", "screen followed link")
            require(not (box.device / "private/run").exists(), "screen started bridge")
            require((box.root / "remote/echo-bridge.lock").exists() == (fault == "locked"), "lock leaked")
            require(box.calls() == [], "direct screen invoked adb")
            ledger = box.root / "screen-ledger"
            if fault in groups["dependency"]:
                failed = [json.loads(line) for line in ledger.read_text().splitlines()]
                require(failed[-1][:2] == {
                    "pm": PACKAGE_QUERY[:2], "install": ["pm", "install"],
                    "grant": GRANT[:2], "am": START[:2], "launch-error": START[:2],
                }[fault], "failed stage did not stop")
                (box.root / "screen-fault").unlink()
                box.run(["--device", "screen-setup"])
                okay = True
            if okay:
                if fault in ("missing", "older", "present"):
                    box.run(["--device", "screen-setup"])
                calls = [json.loads(line) for line in ledger.read_text().splitlines()]
                installs = [call for call in calls if call[:2] == ["pm", "install"]]
                expected = {"present": 0, "install": 2}.get(fault, 1)
                require(len(installs) == expected, "package install count differs")
                require(calls[-2:] == [GRANT, START], "grant and launch order differs")
                if fault in ("older", "present"):
                    require((box.root / "screen-app-data").read_text() == "preserved", "upgrade replaced app data")
            else:
                require(not ledger.exists(), "Android mutations bypassed lock or path checks")
            if okay:
                require(home.read_text() == "org.portos.echoscreen\n", "home selection differs")
                require(home.stat().st_mode & 0o777 == 0o644, "home mode differs")
                require(owner(box.root, home) == "system:system", "home owner differs")
                require(not (state / ".echo-screen-home").exists(), "home stage leaked")
    print(f"screen: {group}=PASS privacy=PASS")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--tool"]:
        sys.exit(android_tool())
    run_cases()
