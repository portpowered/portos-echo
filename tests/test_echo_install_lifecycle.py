"""Held-stdin lifecycle witness through the delivered CLI and controlled adb."""
import contextlib
import json
import os
import selectors
import signal
import shlex
import shutil
import stat
import sys
from pathlib import Path
import subprocess
from unittest.mock import patch
from test_echo_install import Sandbox, prepare_lifecycle, require, owner, set_owner

def ownership_lifetime_case():
    import tempfile
    with tempfile.TemporaryDirectory(prefix="echo-owner-") as directory:
        base = Path(directory)
        path = base / "stage"
        path.touch()
        set_owner(base, path, "9999:9999")
        deleted = path.stat()
        path.unlink()
        path.touch()
        # Deterministically model inode recycling; never wait for an allocator.
        with patch.object(Path, "stat", return_value=deleted):
            require(owner(base, path) == "0:0", "recycled inode inherited deleted ownership")
        set_owner(base, path, "9999:3990")
        published = base / "published"
        path.rename(published)
        os.link(published, path)
        require(owner(base, published) == owner(base, path) == "9999:3990",
                "publication or hardlink lost ownership")
        published.write_text("retained bytes")
        require(owner(base, path) == "9999:3990", "writing retained data reset ownership")
        fifo = base / "stdin"
        os.mkfifo(fifo, 0o600)
        set_owner(base, fifo, "9999:3990")
        require(owner(base, fifo) == "9999:3990", "FIFO ownership transfer lost")
        fifo.unlink()
        os.mkfifo(fifo, 0o600)
        require(owner(base, fifo) == "0:0", "recreated FIFO inherited deleted ownership")
    print("NR ownership-lifetime=PASS")


def recovered_metadata(box):
    for tree in ("private", "home"):
        root = box.device / tree
        for path in [root, *root.rglob("*")]:
            expected = 0o700 if path.is_dir() or path.name == "launch.sh" else 0o600
            require(path.stat().st_mode & 0o777 == expected, "recovery runtime mode differs")
            expected_owner = "9999:3990" if path.name == "credential" else "9999:9999"
            require(owner(box.root, path) == expected_owner, "recovery runtime owner differs")
    for name, mode in (("", 0o755), ("bin", 0o755), ("bin/echo-bridge", 0o755),
                       ("bin/codex-app-server", 0o755), ("bin/cacert.pem", 0o644)):
        path = box.device / name
        require(owner(box.root, path) == "0:0", "recovery executable owner differs")
        require(path.stat().st_mode & 0o777 == mode, "recovery executable mode differs")
def recreate_root_data(box):
    # Control only the deleted object's inode identity, preserving other tests'
    # real stat observations even while their sandboxes run in parallel.
    recycled = box.device / "home/recreated"
    recycled.write_text("new root-owned data")
    deleted = box.root / "deleted-stage"
    deleted.touch()
    recycled_info = recycled.stat()
    real_stat = type(deleted).stat
    def observed_stat(path, *args, **kwargs):
        return recycled_info if path == deleted else real_stat(path, *args, **kwargs)
    with patch.object(type(deleted), "stat", observed_stat):
        set_owner(box.root, deleted, "9999:3990")
    deleted.unlink()
def recovery_case(box, fault):
    box.prompt("voice.md", "default voice")
    prepare_lifecycle(box)
    values = {"private/credential": "retained auth", "home/prompts/voice.md": "owner voice",
              "home/settings.toml": "retained settings"}
    for name, value in values.items():
        (box.device / name).write_text(value)
    set_owner(box.root, box.device / "private/credential", "9999:3990")
    original = (box.device / "private/bridge.toml").read_bytes()
    box.files["config"].write_text("replacement config")
    before = len(box.calls())
    (box.root / "fault").write_text(fault)
    box.install(force=True, code=1)
    require(not (box.root / "fault").exists(), "fault was not consumed")
    require((box.device / "private/bridge.toml").read_bytes() == original, "failed stage lost config")
    for name, value in values.items():
        require((box.device / name).read_text() == value, "failed stage lost retained state")
    require(not (box.device / "private/.install-stage").exists(), "stage leaked")
    require(not (box.root / "remote/echo-bridge.lock").exists(), "failed operation leaked lock")
    require(not any("setsid" in " ".join(call) for call in box.calls()[before:]), "failure launched")
    recreate_root_data(box)
    values["home/recreated"] = "new root-owned data"
    box.install(force=True)
    require((box.device / "private/bridge.toml").read_text() == "replacement config", "recovery force ineffective")
    box.files["config"].write_text("unused config")
    box.install()
    require((box.device / "private/bridge.toml").read_text() == "replacement config", "reinstall lost config")
    for name, value in values.items():
        require((box.device / name).read_text() == value, "recovery lost retained state")
    recovered_metadata(box)
    require(not (box.device / "private/.install-stage").exists(), "recovery stage leaked")
    require(not (box.root / "remote/echo-bridge.lock").exists(), "recovery lock leaked")
    box.run(["start"])
    require((box.root / "runas-user").read_text() == "nobody", "recovered launch identity differs")
    require("process_user=nobody\n" in box.run(["status"]).stdout, "recovered status identity differs")
    require((box.root / "child-session").read_text() == (box.root / "holder-pid").read_text(),
            "recovered launch did not detach")
    box.run(["stop"])
    require((box.root / "eof").read_text() == "EOF", "recovered child did not see EOF")
    require(not (box.device / "private/run").exists(), "recovered stop leaked run state")
def paused_start(box, point):
    for name in ("paused", "release"):
        os.mkfifo(box.root / name)
    (box.root / "pause").write_text(point)
    child = subprocess.Popen(["/bin/sh", str(box.script), "start"], cwd=box.root,
                             env=box.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             text=True, start_new_session=True)
    with os.fdopen(os.open(box.root / "paused", os.O_RDWR | os.O_NONBLOCK), "rb") as ready:
        with selectors.DefaultSelector() as selector:
            selector.register(ready, selectors.EVENT_READ)
            if selector.select(5) and ready.read(6) == b"paused":
                return child
    status = child.poll()
    observed = {name: (box.root / name).exists() for name in ("pause", "runas-user", "child-pid")}
    stage = box.root / "failed-stage"
    with contextlib.suppress(ProcessLookupError):
        os.killpg(child.pid, signal.SIGKILL)
    child.communicate(timeout=5)
    raise AssertionError(f"start did not reach controlled barrier: exit={status}, "
                         f"markers={observed}, failed stage={stage.read_text() if stage.exists() else '-'}")
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
    for user in ("root", "0", "unknown"):
        with Sandbox() as box:
            prepare_lifecycle(box)
            (box.root / "process-user").write_text(user)
            box.run(["start"], 1)
            require((box.root / "eof").read_text() == "EOF",
                    "unsafe startup identity did not close owned stdin")
            box.run(["stop"])
    concurrent_cases()
    interrupted_cases()
    print("F09 F10 F11 F12 F14 F15 F16: PASS")
def nonroot_cases():
    # Runtime ownership is modeled, never changed on the worker host.
    for trigger in ("install", "start"):
        with Sandbox() as box:
            prepare_lifecycle(box)
            values = {"private/credential": "retained auth", "home/settings.toml": "retained settings",
                      "home/prompts/custom.md": "retained prompt"}
            (box.device / "private/codex").mkdir()
            values["private/codex/auth-marker"] = "retained marker"
            for name, value in values.items():
                (box.device / name).write_text(value)
            for path in [box.device / "private", box.device / "home", *box.device.rglob("*")]:
                if "/bin" not in str(path):
                    set_owner(box.root, path, "0:0")
            # A legacy generated launcher must be replaced before privilege drop.
            (box.device / "private/launch.sh").write_text("exit 89\n")
            box.install() if trigger == "install" else box.run(["start"])
            if trigger == "start":
                box.run(["stop"])
            for tree in ("private", "home"):
                root = box.device / tree
                for path in [root, *root.rglob("*")]:
                    require(owner(box.root, path) == "9999:9999", "runtime ownership differs")
                    expected = 0o700 if path.is_dir() or path.name == "launch.sh" else 0o600
                    require(path.stat().st_mode & 0o777 == expected, "runtime mode differs")
            for name in ("bin", "bin/echo-bridge", "bin/codex-app-server", "bin/cacert.pem"):
                require(owner(box.root, box.device / name) == "0:0", "executable ownership differs")
            set_owner(box.root, box.device / "private/credential", "9999:3990")
            before = (box.root / "ownership-ledger").read_text().splitlines()
            box.install() if trigger == "install" else box.run(["start"])
            if trigger == "start":
                require((box.root / "runas-user").read_text() == "nobody", "launch did not drop privilege")
                box.run(["stop"])
            require(owner(box.root, box.device / "private/credential") == "9999:3990",
                    "migration changed an already nobody-owned group")
            delta = (box.root / "ownership-ledger").read_text().splitlines()[len(before):]
            for name, value in values.items():
                path = box.device / name
                require(path.read_text() == value, "migration lost retained bytes")
                require(not any(line.startswith(str(path) + "=") for line in delta), "migration not idempotent")
            require(not any(line.startswith(str(box.device / tree) + "=") for tree in ("private", "home")
                            for line in delta), "repeated migration corrected a directory")
    for trigger in ("install", "start"):
        for failure in ("chown", "symlink", "hardlink", "fifo"):
            with Sandbox() as box:
                prepare_lifecycle(box)
                path = box.device / "private/retained"
                path.write_text("retained bytes")
                set_owner(box.root, path, "0:0")
                outside = box.root / "outside"
                outside.write_text("outside bytes")
                if failure == "chown":
                    (box.root / "fault").write_text("chown")
                elif failure == "symlink":
                    (box.device / "home/nested-link").symlink_to(outside)
                elif failure == "hardlink":
                    os.link(outside, box.device / "private/.install-stage")
                else:
                    os.mkfifo(box.device / "home/unsupported")
                index = len(box.calls())
                box.install(code=1) if trigger == "install" else box.run(["start"], 1)
                require(not any("setsid" in " ".join(call) for call in box.calls()[index:]), "failed migration launched")
                require(path.read_text() == "retained bytes" and outside.read_text() == "outside bytes", "migration escaped")
                require(not (box.root / "remote/echo-bridge.lock").exists(), "migration leaked lock")
    print("NR install=PASS ownership=PASS migration=PASS launch=PASS status=PASS")
def configure_lifecycle(box, mode="ready", bridge=None):
    os.mkfifo(box.root / "ready")
    (box.root / "remote/hassmic/boot.log").write_text("old client connected\n")
    fixture = """import os, sys, signal, json
from pathlib import Path
base = Path(os.environ['FAKE_ROOT'])
(base / 'child-pid').write_text(str(os.getpid()))
(base / 'holder-pid').write_text(str(os.getppid()))
(base / 'child-args').write_text(json.dumps(sys.argv[1:]))
(base / 'child-session').write_text(str(os.getsid(0)))
mode = (base / 'mode').read_text()
log = base / 'remote/hassmic/boot.log'
if mode in ('ready', 'hidden', 'deadline', 'stubborn'):
    with log.open('a') as output: output.write('client connected / satellite: running\\n')
elif mode == 'rotate':
    log.rename(log.with_suffix('.old'))
    log.write_text('client connected\\n')
elif mode == 'truncate':
    log.write_text('client connected\\n')
with (base / 'ready').open('w') as ready: ready.write('ready')
if mode != 'exit':
    sys.stdin.buffer.read()
    (base / 'eof').write_text('EOF')
    if mode == 'stubborn': signal.pause()
"""
    bridge = bridge or box.files["bridge"]
    bridge.write_text("#!" + sys.executable + "\n" + fixture)
    (box.root / "mode").write_text(mode)
    for name in ("date", "sleep"):
        wrapper = box.root / "remote-tools" / name
        wrapper.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " -S "
                           + shlex.quote(str(Path(__file__).with_name("test_echo_install.py").resolve())) + " --tool " + name + ' "$@"\n')
        wrapper.chmod(0o755)
    bridge.chmod(0o755)
def prepared_run(base):
    """Observe real preparation before the launcher's atomic gate is consumed."""
    run = base / "remote/echo-bridge/private/run"
    for name, mode in (("", 0o700), ("control", 0o700), ("stdin", 0o600),
                       ("deadline", 0o600), ("log-inode", 0o600), ("log-prefix", 0o600)):
        path = run / name
        require(owner(base, path) == "9999:9999", "run ownership differs before launch")
        require(stat.S_IMODE(path.stat().st_mode) == mode, "run mode differs before launch")
    require(run.is_dir() and (run / "control").is_dir(), "launch gate missing")
    require(stat.S_ISFIFO((run / "stdin").stat().st_mode), "stdin is not a FIFO")
    require(int((run / "deadline").read_text()) == int((base / "clock").read_text()) + 19,
            "preparation deadline differs")
    log = base / "remote/hassmic/boot.log"
    require((run / "log-inode").read_text().strip() == str(log.stat().st_ino), "log inode not captured")
    with log.open("rb") as source:
        expected = subprocess.check_output([shutil.which("cksum")], stdin=source)
    require((run / "log-prefix").read_bytes() == expected, "log prefix not captured")
def release_started(box, starts=1):
    calls = box.calls()
    launches = [c for c in calls if "setsid " in " ".join(c)]
    expected = ('setsid /data/local/hassmic/runas "nobody" "3990,inet" '
                "/system/bin/sh '/data/local/echo-bridge/private/launch.sh'")
    require(len(launches) == starts and all(expected in c[1] for c in launches), "launch argv/count differs")
    require((box.root / "runas-user").read_text() == "nobody", "release identity differs")
    require((box.root / "child-session").read_text() == (box.root / "holder-pid").read_text(),
            "release launcher did not detach")
    require(not (box.root / "eof").exists(), "release stdin closed before stop")
    require(json.loads((box.root / "child-args").read_text()) ==
            ["--config", str(box.device / "private/bridge.toml"),
             "--trace", str(box.device / "private/bridge.trace")], "release child arguments differ")
def release_stop(box):
    box.run(["stop"])
    require((box.root / "eof").read_text() == "EOF", "release stop did not close stdin")
    require(not (box.device / "private/run").exists(), "release stop leaked state")
    # A later start must prove its own held stdin rather than inherit this marker.
    (box.root / "eof").unlink()


def setup_operator_cases():
    private_connection_cases()
    # Real authored shell; substitute only Android filesystem/tools and bridge process.
    cases = (("signin", "stopped", 0), ("signin", "signed-in", 0), ("link", "nobody", 0), ("link", "9999", 0),
             ("ensure-signin", "stopped", 0), ("ensure-signin", "signed-in", 0),
             ("ensure-signin", "nobody", 1), ("ensure-signin", "locked", 1),
             ("ensure-signin", "dependency", 1), ("ensure-signin", "redirected", 1),
             ("signin", "nobody", 1), ("link", "stopped", 1), ("link", "root", 1),
             ("link", "unavailable", 1), ("signin", "locked", 1),
             ("signin", "dependency", 1), ("signin", "redirected", 1))
    for verb, state, code in cases:
        with Sandbox() as box:
            box.install()
            box.run([verb], 2)  # Private operations never grow the Linux host CLI.
            for name in ("echo-install.sh", "echo-install-release.sh", "echo-install-setup.sh"):
                path = box.script.with_name(name)
                path.write_text(path.read_text().replace("/data/local", str(box.root / "remote")))
            bridge = box.device / "bin/echo-bridge"
            bridge.write_text("#!" + sys.executable + "\n" + """
import json, os, sys
from pathlib import Path
root = Path(os.environ['FAKE_ROOT'])
with (root / 'operator-argv').open('a') as ledger: ledger.write(json.dumps(sys.argv[1:]) + '\\n')
(root / 'operator-user').write_text(os.environ.get('FAKE_UID', 'root'))
assert not sys.stdin.buffer.read(), 'operator stdin was not closed'
print('synthetic-private-content-do-not-print')
print('synthetic-private-content-do-not-print', file=sys.stderr)
sys.exit(1 if (root / 'operator-fails').exists() or
         (sys.argv[1] == 'status' and not (root / 'signed-in').exists()) else 0)
""")
            config = box.device / "private/bridge.toml"
            original = config.read_bytes()
            if state in ("nobody", "9999", "root", "unavailable"):
                (box.root / "running").touch()
                (box.root / "process-user").write_text(state)
            if state == "locked":
                (box.root / "remote/echo-bridge.lock").mkdir()
            if state == "dependency":
                (box.root / "operator-fails").touch()
            if state == "signed-in":
                (box.root / "signed-in").touch()
            if state == "redirected":
                config.unlink()
                config.symlink_to(box.files["config"])
            box.env["PATH"] = str(box.root / "remote-tools") + ":/usr/bin:/bin"
            result = box.run(["--device", verb], code)
            called = box.root / "operator-argv"
            require(called.exists() == (code == 0 or state == "dependency"), "operator admission differs")
            if called.exists():
                verbs = ["status"] if verb == "ensure-signin" else [verb]
                if verb == "ensure-signin" and state != "signed-in":
                    verbs.append("signin")
                require([json.loads(line) for line in called.read_text().splitlines()] ==
                        [[step, "--config", str(config)] for step in verbs], "operator argv/count differs")
                require((box.root / "operator-user").read_text() == "9999", "operator ran as root")
            require(not list(box.root.glob("holder-pid")), "operator started the runtime")
            require((box.root / "remote/echo-bridge.lock").exists() == (state == "locked"), "operator lock leaked")
            if state != "redirected":
                require(config.read_bytes() == original, "operator changed config bytes")
            require(result.stdout == ("echo-install: " + verb + " completed\n" if code == 0 else ""),
                    "operator success/privacy differs")
    print("echo-install setup: signin=PASS resume=PASS link=PASS identity=PASS lock=PASS privacy=PASS")

def private_connection_cases():
    for verb, name in (("setup-config", "bridge.toml"), ("setup-pairing", "screen-pairing.txt")):
        for fault in ("none", "missing", "empty", "oversize", "owner", "mode", "hardlink", "symlink", "parent", "root", "args"):
            with Sandbox() as box:
                for script in box.script.parent.glob("echo-install*.sh"):
                    script.write_text(script.read_text().replace("/data/local", str(box.root / "remote")))
                box.env["PATH"] = str(box.root / "remote-tools") + ":/usr/bin:/bin"
                private = box.device / "private"
                private.mkdir(parents=True); private.chmod(0o700); set_owner(box.root, private, "9999:9999")
                target = private / name
                target.write_text("SYNTHETIC-PRIVATE-PAIR\n"); target.chmod(0o600); set_owner(box.root, target, "9999:9999")
                if fault == "missing": target.unlink()
                elif fault == "empty": target.write_text("")
                elif fault == "oversize": target.write_bytes(b"x" * (1048577 if verb == "setup-config" else 16385))
                elif fault == "owner": set_owner(box.root, target, "0:0")
                elif fault == "mode": target.chmod(0o644)
                elif fault == "hardlink": os.link(target, private / "alias")
                elif fault in ("symlink", "parent", "root"):
                    path = target if fault == "symlink" else private if fault == "parent" else box.device
                    saved = path.with_name(path.name + "-saved")
                    path.rename(saved); path.symlink_to(saved, target_is_directory=fault != "symlink")
                args = ["--device", verb] + (["ignored"] if fault == "args" else [])
                result = box.run(args, 0 if fault == "none" else 1)
                require(result.stdout == ("SYNTHETIC-PRIVATE-PAIR\n" if fault == "none" else "") and not result.stderr,
                        "private connection admission/privacy differs: " + verb + "/" + fault)
                require(not box.calls(), "private device reads invoked host adb")
                require(not (box.root / "remote/echo-bridge.lock").exists(), "read acquired lifecycle lock")
    print("echo-install private connection: reads=PASS unsafe=REFUSED account-files=UNREAD")
