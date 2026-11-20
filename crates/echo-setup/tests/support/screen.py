"""T3 authored device shell; isolated Android tools/files, no adb or hardware."""
import json
import os
from pathlib import Path
import shlex
import sys

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "tests"))
from test_echo_install import Sandbox, require, SENTINEL, owner


def android_tool():
    root = Path(os.environ["FAKE_ROOT"])
    args = sys.argv[2:]
    require((root / "remote/echo-bridge.lock").is_dir(), "screen mutation bypassed lock")
    with (root / "screen-ledger").open("a") as ledger:
        ledger.write(json.dumps(args) + "\n")
    print(SENTINEL, file=sys.stderr)
    if ((root / "screen-fault").exists() and (root / "screen-fault").read_text()
            in (args[0], "install" if args[:2] == ["pm", "install"] else "unused")):
        return 1
    if args[:3] == ["pm", "list", "packages"]:
        require(args[3:] == ["org.portos.echoscreen"], "package query differs")
        if (root / "screen-present").exists():
            print("package:org.portos.echoscreen")
        return 0
    if args[:3] == ["pm", "install", "-r"]:
        require(Path(args[3]).read_bytes() == b"synthetic APK", "wrong APK installed")
        (root / "screen-present").touch()
        print("Success")
        return 0
    require(args == ["am", "start", "-n", "org.portos.echoscreen/.MainActivity"], "activity differs")
    print(SENTINEL)
    return 0


def fixture(box):
    box.install()
    apk = box.device / "apk/echo-screen.apk"
    apk.parent.mkdir()
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
    (box.root / "ledger").unlink()
    return state


def run_cases():
    groups = {"lifecycle": ("missing", "present", "locked"),
              "dependency": ("pm", "install", "am"),
              "paths": ("home-link", "stage-link", "hardlink", "parent-link")}
    group = sys.argv[1]
    for fault in groups[group]:
        with Sandbox() as box:
            state = fixture(box)
            home = state / "screen_home"
            outside = box.root / "untouched"
            outside.write_text("untouched")
            if fault == "present":
                (box.root / "screen-present").touch()
            if fault in ("pm", "install", "am"):
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
            okay = fault in ("missing", "present")
            result = box.run(["--device", "screen-setup"], 0 if okay else 1)
            require(result.stdout == ("echo-install: screen ready\n" if okay else ""), "screen result differs")
            require(outside.read_text() == "untouched", "screen followed link")
            require(not (box.device / "private/run").exists(), "screen started bridge")
            require((box.root / "remote/echo-bridge.lock").exists() == (fault == "locked"), "lock leaked")
            require(box.calls() == [], "direct screen invoked adb")
            if fault in ("pm", "install", "am"):
                (box.root / "screen-fault").unlink()
                box.run(["--device", "screen-setup"])
                okay = True
            if okay:
                calls = [json.loads(line) for line in (box.root / "screen-ledger").read_text().splitlines()]
                installs = [call for call in calls if call[:2] == ["pm", "install"]]
                expected = {"present": 0, "install": 2}.get(fault, 1)
                require(len(installs) == expected, "package install count differs")
                require(home.read_text() == "org.portos.echoscreen\n", "home selection differs")
                require(home.stat().st_mode & 0o777 == 0o644, "home mode differs")
                require(owner(box.root, home) == "system:system", "home owner differs")
                require(not (state / ".echo-screen-home").exists(), "home stage leaked")
    print(f"screen: {group}=PASS privacy=PASS")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--tool"]:
        sys.exit(android_tool())
    run_cases()
