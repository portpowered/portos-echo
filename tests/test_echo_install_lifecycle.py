"""Held-stdin lifecycle witness through the delivered CLI and controlled adb."""
import json
import os
import selectors
import signal
import subprocess
import hashlib
import io
import tarfile
from unittest.mock import patch

from test_echo_install import Sandbox, prepare_lifecycle, require, owner, set_owner


def device_archive(box, kind):
    """An explicit Android filesystem/tool boundary, without any real adb."""
    source = box.script.read_text().replace("/data/local", str(box.root / "remote"))
    source = source.replace("/system/bin/sh", "/bin/sh")
    box.script.write_text(source)
    stage = box.root / "remote/tmp/echo-setup-1-2"
    stage.mkdir(parents=True, mode=0o700)
    payload = dict.fromkeys(("MANIFEST", "LICENSE", "NOTICE", "bin/echo-bridge",
                            "bin/codex-app-server", "bin/cacert.pem", "apk/echo-screen.apk",
                            "examples/bridge.echo.toml", "examples/prompts/voice.md",
                            "examples/prompts/runner.md", "licenses/codex/LICENSE",
                            "licenses/codex/NOTICE", "licenses/ca/MPL-2.0.txt",
                            "licenses/ca/NOTICE"), b"synthetic public bytes")
    payload["scripts/echo-install.sh"] = source.encode()
    helper = box.script.with_name("echo-install-release.sh")
    helper.write_text(helper.read_text().replace("/data/local", str(box.root / "remote")))
    payload["scripts/echo-install-release.sh"] = helper.read_bytes()
    archive = stage / "archive.tar.gz"
    with tarfile.open(archive, "w:gz", format=tarfile.USTAR_FORMAT) as target:
        for name, data in payload.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            target.addfile(member, io.BytesIO(data))
        if kind != "valid":
            name = {"parent": "../escaped", "absolute": "/escaped", "unexpected": "unexpected",
                    "duplicate": "LICENSE", "symlink": "licenses/link", "fifo": "licenses/pipe"}[kind]
            member = tarfile.TarInfo(name)
            if kind == "symlink":
                member.type, member.linkname = tarfile.SYMTYPE, "../../escaped"
            elif kind == "fifo":
                member.type = tarfile.FIFOTYPE
            target.addfile(member, io.BytesIO())
    return stage, archive


def device_transport_cases():
    for kind in ("valid", "parent", "absolute", "unexpected", "duplicate", "symlink", "fifo"):
        with Sandbox() as box:
            stage, archive = device_archive(box, kind)
            env = dict(box.env, PATH=str(box.root / "remote-tools") + ":/usr/bin:/bin")
            args = ["/bin/sh", str(box.script), "--device", "prepare-archive", "--archive",
                    str(archive), "--sha256", hashlib.sha256(archive.read_bytes()).hexdigest(),
                    "--stage", str(stage)]
            result = subprocess.run(args, env=env, capture_output=True, timeout=5)
            require(result.returncode == (0 if kind == "valid" else 1), "direct archive refusal differs")
            require(not box.device.exists(), "archive preparation mutated installed state")
            require(not (stage / "escaped").exists(), "archive escaped its bundle")
            if kind != "valid":
                require(not (stage / "bundle").exists(), "unsafe archive extracted before validation")
                continue
            bundle = stage / "bundle"
            result = subprocess.run(["/bin/sh", str(bundle / "scripts/echo-install.sh"),
                                     "--device", "install", "--bundle", str(bundle), "--release-version", "0.2.0"],
                                    env=env, capture_output=True, timeout=10)
            require(result.returncode == 0, "direct bundled install failed")
            require((box.device / "bin/echo-bridge").read_bytes() == b"synthetic public bytes",
                    "direct install bytes differ")
            require(owner(box.root, box.device / "private/bridge.toml") == "9999:9999",
                    "direct install changed private owner")
            require(box.calls() == [], "direct transport invoked adb")
    print("echo-install direct transport: install=PASS archive-safety=PASS")


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


def public_bytes(box):
    return {str(path.relative_to(box.device)): path.read_bytes()
            for name in ("bin", "apk", "scripts", "MANIFEST")
            for path in ([box.device / name] if (box.device / name).is_file()
                         else (box.device / name).rglob("*")) if path.is_file()}


def release_cases():
    # Run both the actual bundle device entry and ordinary Linux operator CLI.
    with Sandbox() as box:
        stage, archive = device_archive(box, "valid")
        env = dict(box.env, PATH=str(box.root / "remote-tools") + ":/usr/bin:/bin")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        result = subprocess.run(["/bin/sh", str(box.script), "--device", "prepare-archive",
                                 "--archive", str(archive), "--sha256", digest, "--stage", str(stage)],
                                env=env, capture_output=True, timeout=5)
        require(result.returncode == 0, "release archive preparation failed")
        bundle = stage / "bundle"

        def install(version):
            result = subprocess.run(["/bin/sh", str(bundle / "scripts/echo-install.sh"),
                                     "--device", "install", "--bundle", str(bundle),
                                     "--release-version", version], env=env,
                                    capture_output=True, timeout=15)
            require(result.returncode == 0, "release install failed: " + result.stderr.decode())

        install("0.1.0")
        original = public_bytes(box)
        box.run(["rollback"], 1)
        require(public_bytes(box) == original, "first rollback changed active release")
        private = {"private/bridge.toml": "edited config", "home/notes.md": "retained notes"}
        for name, value in private.items():
            path = box.device / name
            path.write_text(value)
            set_owner(box.root, path, "9999:9999")
        for name in ("bin/echo-bridge", "bin/codex-app-server", "bin/cacert.pem", "apk/echo-screen.apk", "MANIFEST"):
            (bundle / name).write_text("version B: " + name)
        install("0.2.0")
        current = public_bytes(box)
        require(json.loads((box.device / "release.json").read_text()) ==
                {"format": 1, "current": "0.2.0", "previous": "0.1.0"}, "release metadata differs")
        previous = box.device / "releases/previous"
        require({str(path.relative_to(previous)): path.read_bytes()
                 for path in previous.rglob("*") if path.is_file() and path.name not in ("version", "SHA256SUMS")}
                == original, "previous payload differs")
        require(not (previous / "private").exists() and not (previous / "home").exists(),
                "snapshot contains private data")
        saved = (previous / "bin/echo-bridge").read_bytes()
        (previous / "bin/echo-bridge").write_bytes(b"tampered")
        box.run(["rollback"], 1)
        require(public_bytes(box) == current, "tampered rollback changed active files")
        (previous / "bin/echo-bridge").write_bytes(saved)
        box.run(["rollback"])
        require(public_bytes(box) == original, "rollback did not restore every public file")
        require(json.loads((box.device / "release.json").read_text()) ==
                {"format": 1, "current": "0.1.0", "previous": None}, "rollback version differs")
        for name, value in private.items():
            require((box.device / name).read_text() == value, "rollback changed private/home bytes")
        box.run(["rollback"], 1)
        require(not (box.root / "remote/echo-bridge.lock").exists(), "rollback leaked lock")
        box.run(["uninstall"])
        require(not (box.device / "releases").exists(), "uninstall retained release binaries")
    with Sandbox() as box:
        box.install()
        original = public_bytes(box)
        box.files["bridge"].write_text("version B")
        (box.root / "fault").write_text("push")
        box.install(code=1)
        require((box.device / "releases/pending").is_file(), "partial install is not marked")
        box.run(["status"], 1)
        box.run(["start"], 1)
        box.run(["rollback"])
        require(public_bytes(box) == original, "failed update lost recoverable files")
        box.files["bridge"].write_text("version C")
        box.install()
        require((box.device / "bin/echo-bridge").read_text() == "version C", "recovery did not install")
    print("echo-install releases: snapshot=PASS rollback=PASS partial=PASS retention=PASS")
