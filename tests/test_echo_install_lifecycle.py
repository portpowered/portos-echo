"""Held-stdin lifecycle witness through the delivered CLI and controlled adb."""
import json
import os
import selectors
import signal
import subprocess

from test_echo_install import Sandbox, prepare_lifecycle, require


def paused_start(box, point):
    for name in ("paused", "release"):
        os.mkfifo(box.root / name)
    (box.root / "pause").write_text(point)
    fd = os.open(box.root / "paused", os.O_RDWR | os.O_NONBLOCK)
    child = subprocess.Popen(["/bin/sh", str(box.script), "start"], cwd=box.root,
                             env=box.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    with selectors.DefaultSelector() as selector:
        selector.register(fd, selectors.EVENT_READ)
        require(bool(selector.select(5)), "start did not reach controlled barrier")
        os.read(fd, 100)
    os.close(fd)
    return child


def release_start(box, child, code):
    with (box.root / "release").open("w") as release:
        release.write("released")
    out, err = child.communicate(timeout=10)
    require(child.returncode == code, "paused start exit differs: " + err)
    require(not out if code else out == "echo-install: started (hassmic connected)\n", "paused start result differs")


def concurrent_cases():
    for rival in ("start", "install", "uninstall"):
        with Sandbox() as box:
            prepare_lifecycle(box)
            child = paused_start(box, "read -r end <")
            before = len(box.calls())
            result = box.run(box.install_args() if rival == "install" else [rival], 1)
            require(result.stderr == "echo-install: device operation failed\n",
                    "busy caller result differs")
            require(not any("setsid" in " ".join(call) or call[0] == "push"
                            for call in box.calls()[before:]), "concurrent mutator passed lock")
            require((box.device / "bin/echo-bridge").is_file(), "concurrent removal")
            release_start(box, child, 0)
            require(sum("setsid" in " ".join(call) for call in box.calls()) == 1,
                    "concurrent launch count differs")
            box.run(["stop"])
    print("F13: PASS (start/start, start/install and start/uninstall)")


def interrupted_cases():
    for point in ("setsid", "read -r end <"):
        with Sandbox() as box:
            prepare_lifecycle(box)
            child = paused_start(box, point)
            child.send_signal(signal.SIGTERM)
            release_start(box, child, 1)
            require(not (box.device / "private/run").exists(), "cancelled start leaked run state")
            box.run(["start"])
            box.run(["stop"])
    with Sandbox() as box:
        prepare_lifecycle(box)
        (box.root / "fault").write_text("setsid")
        box.run(["start"], 1)
        require(not (box.device / "private/run").exists(), "failed launch leaked run state")
        box.run(["start"])
        box.run(["stop"])
    print("F17: PASS")


def lifecycle_cases():
    with Sandbox() as box:
        prepare_lifecycle(box)
        result = box.run(["start"])
        require(result.stdout == "echo-install: started (hassmic connected)\n", "start output differs")
        require(not (box.root / "eof").exists(), "stdin closed before stop")
        require((box.root / "child-session").read_text() == (box.root / "holder-pid").read_text(),
                "launcher did not detach its session")
        args = json.loads((box.root / "child-args").read_text())
        require(args == ["--config", str(box.device / "private/bridge.toml"),
                         "--trace", str(box.device / "private/bridge.trace")], "launch arguments differ")
        before = len(box.calls())
        box.run(["start"], 1)
        box.install(code=1)
        require(not any(call[0] == "push" or "setsid" in " ".join(call)
                        for call in box.calls()[before:]), "duplicate launch/replacement")
        box.run(["stop"])
        require((box.root / "eof").read_text() == "EOF", "bridge did not observe EOF")
        require(not (box.device / "private/run").exists(), "owned resources remained")
        box.run(["stop"])
    for mode in ("stale", "rotate", "truncate", "exit", "hidden", "deadline"):
        with Sandbox() as box:
            prepare_lifecycle(box, mode)
            result = box.run(["start"], 1)
            require(not result.stdout, "false start success")
            box.run(["stop"])
            require(not (box.device / "private/run").exists(), "failed start resource leak")
    with Sandbox() as box:
        prepare_lifecycle(box)
        (box.root / "running").touch()
        before = len(box.calls())
        box.run(["stop"], 1)
        require(not any("kill " in " ".join(call) for call in box.calls()[before:]),
                "unknown process signalled")
        require((box.device / "bin/echo-bridge").exists(), "failed stop removed binary")
    with Sandbox() as box:
        prepare_lifecycle(box)
        lock = box.root / "remote/echo-bridge.lock"
        lock.mkdir()
        box.run(["start"], 1)
        require(not (box.root / "child-pid").exists(), "busy operation launched")
    with Sandbox() as box:
        prepare_lifecycle(box)
        box.run(["start"])
        (box.root / "fault").write_text("ps -A")
        box.run(["stop"], 1)
        require((box.device / "bin/echo-bridge").exists(), "failed verification removed binary")
        (box.root / "fault").write_text("ps -A")
        box.run(["uninstall", "--purge"], 1)
        require((box.device / "private/bridge.toml").exists(), "failed uninstall observation removed config")
        box.run(["stop"])
    with Sandbox() as box:
        prepare_lifecycle(box, "stubborn")
        box.run(["start"])
        box.run(["stop"], 1)
        require((box.root / "eof").read_text() == "EOF", "persistent child did not see EOF")
        require((box.device / "private/run").is_dir(), "persistent child lost ownership state")
        before = len(box.calls())
        box.run(["start"], 1)
        box.install(code=1)
        box.run(["uninstall", "--purge"], 1)
        require((box.device / "private/bridge.toml").exists(), "failed stop removed private state")
        require(not any(call[0] == "push" or "kill " in " ".join(call)
                        for call in box.calls()[before:]), "persistent child replaced or signalled")
    concurrent_cases()
    interrupted_cases()
    print("F09 F10 F11 F12 F14 F15 F16: PASS")
