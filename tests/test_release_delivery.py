"""Consume a prebuilt installer and bundled shell against isolated device boundaries."""
from contextlib import contextmanager
import errno
import json
import os
from pathlib import Path
import selectors
import shlex
import sys

from test_echo_install import Sandbox, SENTINEL, fake_adb, metadata_tool, owner, remote_tool, require, set_owner


DEVICE = "/data/local"
SERIAL = "synthetic-device-private"
PAIRING = "synthetic-pairing-private"
TRACE = '{"elapsed_ms":1,"event":"wyoming.run_started","provider_auth_mode":"chatgpt","run_id":"run-1"}\n'


class DeliveryBox(Sandbox):
    def __init__(self, binary, assets, archive, release):
        import time
        from test_echo_install_lifecycle import configure_lifecycle

        super().__init__()
        self.binary, self.assets, self.archive, self.release = binary, assets, archive, release
        self.responses = {}
        self.requests = []
        self.env.update(PATH=str(self.tools), NO_PROXY="127.0.0.1")
        (self.root / "devices").write_text(SERIAL + "\tdevice\n")
        (self.root / "remote/tmp").mkdir()
        (self.root / "remote/hassmic/state").mkdir()
        (self.root / "clock").write_text(str(int(time.time())))
        configure_lifecycle(self, bridge=self.root / "synthetic-bridge")
        os.mkfifo(self.root / "stop-ready")
        self.tool(self.tools / "adb", "adb")
        for name in ("pm", "am", "nc", "df", "date", "sleep"):
            self.tool(self.root / "remote-tools" / name, name)
        self.tool(self.root / "remote/hassmic/runas", "runas")

    def tool(self, path, name):
        script = shlex.quote(str(Path(__file__).resolve()))
        path.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} -S {script} --fake {name} "$@"\n')
        path.chmod(0o755)

    def run_setup(self, *args, json_mode=True):
        import subprocess

        completed = self.root / "last-completed"
        completed.unlink(missing_ok=True)
        try:
            result = subprocess.run([str(self.binary), args[0], "--adb", str(self.tools / "adb"),
                                     *(["--json"] if json_mode else []), *args[1:]], cwd=self.root, env=self.env,
                                    capture_output=True, text=True, timeout=75)
        except subprocess.TimeoutExpired:
            stage = completed.read_text() if completed.exists() else "no-adb"
            raise AssertionError(f"delivered {args[0]} exceeded 75s; last successful adb stage={stage}") from None
        public = result.stdout + result.stderr
        for marker in (SENTINEL, SERIAL, PAIRING, str(self.root)):
            require(marker not in public, "delivered installer disclosed private fixture data")
        return result

    def invoke(self, *args):
        result = self.run_setup(*args)
        require(result.returncode == 0 and result.stderr == "", "delivered installer failed: " + result.stdout)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        require([event["type"] for event in events] ==
                (["instruction", "result"] if args[0] in ("install", "update") else ["result"]),
                "delivered output sequence differs")
        require(events[-1]["ok"] is True and events[-1]["command"] == args[0], "delivered result differs")
        if len(events) == 2:
            require(events[0]["message"] == "Say Computer near the Echo. Waiting for a fresh wake and bridge run."
                    and events[-1]["setup"] == "complete", "fresh wake/setup result differs")
        return events[-1]


@contextmanager
def local_endpoint(box, release=False):
    # The same file is the entry point for each fake adb and device-tool call.
    # Only the fixture driver needs the HTTP server and its imports.
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    import threading

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            status, data = self.response()
            self.send_response(status)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def response(self):
            if release:
                prefix = f"/v{box.release}/"
                leaf = self.path.removeprefix(prefix)
                path = box.assets / leaf
                status = box.responses.get(leaf, 200 if self.path == prefix + Path(leaf).name and path.is_file() else 404)
                box.requests.append((self.path, status))
                return status, path.read_bytes() if status == 200 else b""
            if self.path == "/healthz":
                return 200, b'{"status":"running","wyoming":"connected","live":"idle","relink_required":false,"kill_switch":false}'
            if self.path == "/settings/v1?token=" + PAIRING:
                return 200, b'{"version":1,"link_status":"linked"}'
            return 404, b""

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thread.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            thread.join(timeout=5)


def record_forward(base, args):
    path = base / "forwards.json"
    active = json.loads(path.read_text()) if path.exists() else {}
    if args == ["--list"]:
        for local, remote in active.items():
            print(SERIAL, local, remote)
        return 0
    if args[:1] == ["--remove"]:
        del active[args[1]]
    else:
        require(args[:2] == ["--no-rebind", "tcp:0"], "unexpected forward operation")
        ports = json.loads((base / "ports.json").read_text())
        local = f"tcp:{ports[args[2]]}"
        require(local not in active, "rebound a live forward")
        active[local] = args[2]
        print(ports[args[2]])
    path.write_text(json.dumps(active))
    return 0


def remote_path(base, value):
    return value.replace(DEVICE, str(base / "remote")).replace("/system/bin/sh", "/bin/sh")


def run_bundled(base, args):
    import subprocess
    import tempfile

    script = Path(remote_path(base, args[1]))
    source = script.read_text()
    if script.name == "echo-install.sh":
        source = "".join(script.with_name(name).read_text() + "\n" for name in
                         ("echo-install-release.sh", "echo-install-setup.sh")) + source
    # Translate Android filesystem paths at the adb boundary. Keep original
    # verified files intact for shell snapshot hashing and publication.
    with tempfile.NamedTemporaryFile(mode="w", dir=script.parent) as translated:
        translated.write(remote_path(base, source).replace("/proc/", str(base / "proc") + "/"))
        translated.flush()
        argv = ["/bin/sh", translated.name, *(remote_path(base, arg) for arg in args[2:])]
        env = dict(os.environ, PATH=str(base / "remote-tools") + ":/usr/bin:/bin")
        return subprocess.run(argv, cwd=base, env=env, timeout=35).returncode


def append_wake(base):
    with (base / "remote/hassmic/boot.log").open("a") as output:
        print("wake: COMPUTER type=2\nclient connected / satellite: running", file=output)
    with (base / "remote/echo-bridge/private/bridge.trace").open("a") as output:
        output.write(TRACE)


def delivery_calls(base):
    path = base / "delivery-ledger"
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def adb_stage(selected):
    if selected[0] == "push":
        return "push-" + (Path(selected[2]).name if Path(selected[2]).name in
                          ("bundle", "archive.tar.gz", "adapter.sh", "config") else "member")
    if selected[0] != "shell":
        return selected[0]
    command = shlex.split(selected[1])
    if "--device" in command:
        return command[command.index("--device") + 1]
    return "stage-directory" if command[:1] == ["test"] else "shell"


def recording_adb(base, args):
    with (base / "delivery-ledger").open("a") as ledger:
        print(json.dumps(args), file=ledger)
    selected = args[2:] if args[:2] == ["-s", SERIAL] else args
    code = dispatch_adb(base, args, selected)
    if code == 0:
        (base / "last-completed").write_text(adb_stage(selected))
    return code


def dispatch_adb(base, args, selected):
    if selected[:1] == ["forward"]:
        return record_forward(base, selected[1:])
    if selected[:1] == ["push"] and Path(selected[1]).is_dir():
        import shutil

        source = Path(selected[1])
        destination = Path(remote_path(base, selected[2]))
        require(destination.parent.is_dir() and destination.name == "bundle",
                "offline directory push did not target the owned stage")
        shutil.copytree(source, destination / source.name if destination.is_dir() else destination)
        return 0
    if selected[:1] == ["shell"]:
        command = shlex.split(selected[1])
        if command[:4] == ["sh", "-s", "--", "--device"]:
            if command[4] == "confirm-check":
                append_wake(base)
            # The embedded adapter is read from stdin by the original adb fake.
        elif command[:1] == ["sh"] and command[2:3] == ["--device"]:
            return run_bundled(base, command)
    sys.argv[2:] = args
    return fake_adb()


def wait_for_launch(base):
    # Direct-device shell calls run behind one adb invocation. Use its first
    # readiness poll to wait for the fixture child's existing FIFO signal.
    run = base / "remote/echo-bridge/private/run"
    deadline = run / "deadline"
    if not deadline.exists() or (run / "stop").exists() or (run / "finished").exists():
        return
    attempt = deadline.read_text()
    ready = base / "ready-attempt"
    if not attempt or (ready.exists() and ready.read_text() == attempt):
        return
    with open(base / "ready", "rb+", buffering=0) as fifo, selectors.DefaultSelector() as selector:
        selector.register(fifo, selectors.EVENT_READ)
        require(bool(selector.select(5)), "delivered bridge did not signal readiness")
        require(fifo.read(5) == b"ready", "delivered readiness signal differs")
    ready.write_text(attempt)


def fake_tool(base, label, args):
    stop = base / "remote/echo-bridge/private/run/stop"
    notification = base / "stop-ready"
    if label == "sleep":
        if os.environ.get("FAKE_UID") == "9999":
            with open(notification, "rb+", buffering=0) as fifo, selectors.DefaultSelector() as selector:
                if not stop.exists():
                    selector.register(fifo, selectors.EVENT_READ)
                    require(bool(selector.select(20)), "delivered bridge did not receive stop")
                    fifo.read(1)
        return remote_tool()
    if label == "date":
        if stop.exists():
            try:
                descriptor = os.open(notification, os.O_WRONLY | os.O_NONBLOCK)
                with os.fdopen(descriptor, "wb") as fifo:
                    fifo.write(b"s")
            except OSError as error:
                if error.errno not in (errno.ENXIO, errno.EPIPE):
                    raise
        wait_for_launch(base)
        return remote_tool()
    if label == "runas":
        require(args[:2] == ["nobody", "3990,inet"], "runtime identity differs")
        if args[2:4] == [str(base / "remote/echo-bridge/bin/echo-bridge"), "status"]:
            private = base / "remote/echo-bridge/private"
            for leaf, value in (("screen-pairing.txt", PAIRING + "\n"), ("bridge.trace", TRACE)):
                path = private / leaf
                path.write_text(value)
                path.chmod(0o600)
                set_owner(base, path, "9999:9999")
            return 0
        expected = base / "expected-pack"
        if expected.exists():
            identity, digest, members = json.loads(expected.read_text())
            assert_installed_pack(base, identity, digest, {name: bytes.fromhex(data) for name, data in members.items()})
        return metadata_tool(base, label, args)
    if label == "nc":
        require(args == ["-z", "-w", "5", "chatgpt.com", "443"], "unexpected egress probe")
    elif label == "df":
        print("Filesystem blocks used available mount\nfixture 524288 0 524288 /fixture")
    elif label == "pm" and args[:3] == ["list", "packages", "org.portos.echoscreen"]:
        if (base / "apk-present").exists():
            print("package:org.portos.echoscreen")
    elif label == "pm" and args[:2] == ["install", "-r"]:
        require(Path(args[2]).is_file(), "installed APK absent")
        (base / "apk-present").touch()
    else:
        require(label == "am" and args == ["start", "-n", "org.portos.echoscreen/.MainActivity"], "Android call differs")
    return 0


def offline_update(box, release):
    from hashlib import sha256
    import tarfile

    bundle = box.root / "update"
    bundle.mkdir()
    with tarfile.open(box.archive) as archive:
        archive.extractall(bundle, filter="data")
    path = bundle / "bin/echo-bridge"
    with path.open("ab") as binary:
        binary.write(b"fixture update bytes")
    manifest_path = bundle / "MANIFEST"
    manifest = json.loads(manifest_path.read_text())
    manifest["version"] = release
    manifest["files"]["bin/echo-bridge"] = sha256(path.read_bytes()).hexdigest()
    manifest_path.write_text(json.dumps(manifest))
    return bundle, manifest


def public_hashes(box, manifest):
    from hashlib import sha256

    for name, expected in manifest["files"].items():
        if name.split("/", 1)[0] in ("bin", "apk", "scripts"):
            require(sha256((box.device / name).read_bytes()).hexdigest() == expected, "installed public bytes differ")


def delivered_journey(binary, assets, archive, manifest, *, pin_version=False, check_maintenance=True):
    from hashlib import sha256
    import tarfile

    release = manifest["version"]
    update = "0.0.0-delivery" if release != "0.0.0-delivery" else "0.0.0-delivery.2"
    with DeliveryBox(binary, assets, archive, release) as box:
        with local_endpoint(box) as screen, local_endpoint(box) as health, local_endpoint(box, True) as release_port:
            (box.root / "ports.json").write_text(json.dumps({"tcp:16900": screen, "tcp:16901": health}))
            version = ("--version", release) if pin_version else ()
            result = box.invoke("install", *version, "--release-base", f"http://127.0.0.1:{release_port}")
            require(result["version"] == release, "default install version differs")
            asset = f"default-{release}.screenpack.tar.gz"
            require(result["screen_pack_asset"] == asset, "default install asset differs")
            requests = [(f"/v{release}/{name}", 200) for name in ("SHA256SUMS", archive.name, asset)]
            require(box.requests == requests, "release asset requests differ")
            digest = sha256((assets / asset).read_bytes()).hexdigest()
            with tarfile.open(assets / asset) as pack:
                members = {entry.name: pack.extractfile(entry).read() for entry in pack}
            assert_installed_pack(box.root, f"default@{release}", digest, members)
            public_hashes(box, manifest)
            bundle, updated = offline_update(box, update)
            first_update_call = len(delivery_calls(box.root))
            result = box.invoke("update", "--from-dir", str(bundle))
            require(result["version"] == update and "screen_pack_asset" not in result, "offline update result differs")
            pushes = [call[-1] for call in delivery_calls(box.root)[first_update_call:] if call[2:3] == ["push"]]
            require([Path(path).name for path in pushes] == ["adapter.sh", "bundle"],
                    "offline update split its verified directory into separate transfers")
            print(f"F33 offline update: members={len(updated['files']) + 1} directory_pushes=1 stage=complete",
                  flush=True)
            public_hashes(box, updated)
            status = box.invoke("status")
            require(status["versions"] == {"current": update, "previous": release}
                    and status["process_user"] == "nobody" and status["hassmic_link"] == "connected",
                    "updated status differs")
            if check_maintenance:
                require(box.invoke("doctor")["checks"] == [{"name": name, "ok": True, "fix": None} for name in
                        ("adb", "device", "hassmic", "egress", "clock", "disk_free", "process_user")],
                        "delivered doctor observation differs")
            box.invoke("rollback")
            public_hashes(box, manifest)
            require(box.invoke("status")["versions"] == {"current": release, "previous": None},
                    "rollback did not restore previous release")
            require(box.requests == requests, "offline lifecycle requested release assets")
            assert_installed_pack(box.root, f"default@{release}", digest, members)
            require(not any((box.root / "remote/tmp").iterdir()), "staging was retained")
            require(json.loads((box.root / "forwards.json").read_text()) == {}, "owned forwards were retained")
            calls = [call[-1] for call in delivery_calls(box.root)]
            for primitive in ("prepare-archive", "--device install --bundle", "--device screen-setup",
                              "--device start", "--device rollback"):
                require(any(primitive in call for call in calls), "shell dispatch missing " + primitive)
            if check_maintenance:
                box.invoke("uninstall")
    operations = "install/setup/update/status/doctor/rollback" if check_maintenance else "install/setup/update/status/rollback"
    print(f"F33 delivered native {operations}: PASS")


def assert_installed_pack(base, identity, digest, members):
    screens = base / "remote/echo-bridge/home/screens"
    root = screens / identity
    actual = {str(path.relative_to(root)): path.read_bytes() for path in root.rglob("*") if path.is_file()}
    require(actual == members, "installed screen pack members differ")
    marker = screens / f".{identity}.sha256"
    require(marker.read_text() == digest, "installed provenance differs")
    for path in (screens, root, marker, *root.rglob("*")):
        mode = 0o700 if path.is_dir() else 0o600
        require(owner(base, path) == "9999:9999" and path.stat().st_mode & 0o777 == mode,
                "pack is not readable as the bridge owner with expected modes")


if __name__ == "__main__":
    require(sys.argv[1:2] == ["--fake"], "only a test boundary entry point")
    base = Path(os.environ["FAKE_ROOT"])
    label, *args = sys.argv[2:]
    if label == "adb":
        sys.exit(recording_adb(base, args))
    sys.exit(fake_tool(base, label, args))
