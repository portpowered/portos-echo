"""Consume a prebuilt installer and bundled shell against isolated device boundaries."""
from contextlib import contextmanager
from functools import partial
import http.server
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time

from test_echo_install import Sandbox, SENTINEL, fake_adb, metadata_tool, remote_tool, require, set_owner
from test_echo_install_lifecycle import configure_lifecycle
from test_release_kit import sha


DEVICE = "/data/local"
SERIAL = "synthetic-device-private"
PAIRING = "synthetic-pairing-private"
TRACE = '{"elapsed_ms":1,"event":"wyoming.run_started","provider_auth_mode":"chatgpt","run_id":"run-1"}\n'


class DeliveryBox(Sandbox):
    def __init__(self, binary, assets, archive, release):
        super().__init__()
        self.binary, self.assets, self.archive, self.release = binary, assets, archive, release
        self.env.update(PATH=str(self.tools), NO_PROXY="127.0.0.1")
        (self.root / "devices").write_text(SERIAL + "\tdevice\n")
        (self.root / "remote/tmp").mkdir()
        (self.root / "remote/hassmic/state").mkdir()
        (self.root / "clock").write_text(str(int(time.time())))
        configure_lifecycle(self, bridge=self.root / "synthetic-bridge")
        self.tool(self.tools / "adb", "adb")
        for name in ("pm", "am", "nc", "df", "date"):
            self.tool(self.root / "remote-tools" / name, name)
        self.tool(self.root / "remote/hassmic/runas", "runas")

    def tool(self, path, name):
        script = shlex.quote(str(Path(__file__).resolve()))
        path.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} {script} --fake {name} "$@"\n')
        path.chmod(0o755)

    def invoke(self, *args):
        completed = self.root / "last-completed"
        completed.unlink(missing_ok=True)
        try:
            result = subprocess.run([str(self.binary), args[0], "--adb", str(self.tools / "adb"),
                                     "--json", *args[1:]], cwd=self.root, env=self.env,
                                    capture_output=True, text=True, timeout=75)
        except subprocess.TimeoutExpired:
            stage = completed.read_text() if completed.exists() else "no-adb"
            raise AssertionError(f"delivered {args[0]} exceeded 75s; last successful adb stage={stage}") from None
        public = result.stdout + result.stderr
        for marker in (SENTINEL, SERIAL, PAIRING, str(self.root)):
            require(marker not in public, "delivered installer disclosed private fixture data")
        require(result.returncode == 0 and result.stderr == "", "delivered installer failed: " + public)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        require([event["type"] for event in events] ==
                (["instruction", "result"] if args[0] in ("install", "update") else ["result"]),
                "delivered output sequence differs")
        require(events[-1]["ok"] is True and events[-1]["command"] == args[0], "delivered result differs")
        if len(events) == 2:
            require(events[0]["message"] == "Say Computer near the Echo. Waiting for a fresh wake and bridge run."
                    and events[-1]["setup"] == "complete", "fresh wake/setup result differs")
        return events[-1]


class ReleaseHandler(http.server.BaseHTTPRequestHandler):
    def __init__(self, box, *args, **kwargs):
        self.box = box
        super().__init__(*args, **kwargs)

    def log_message(self, *args):
        pass

    def do_GET(self):
        box = self.box
        if self.path == "/healthz":
            data = b'{"status":"running","wyoming":"connected","live":"idle","relink_required":false,"kill_switch":false}'
        elif self.path == "/settings/v1?token=" + PAIRING:
            data = b'{"version":1,"link_status":"linked"}'
        elif self.path in (f"/v{box.release}/SHA256SUMS", f"/v{box.release}/{box.archive.name}"):
            leaf = self.path.rsplit("/", 1)[1]
            with (box.root / "downloads").open("a") as ledger:
                print(leaf, file=ledger)
            data = (box.assets / leaf).read_bytes()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


@contextmanager
def local_endpoint(box):
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), partial(ReleaseHandler, box)) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
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
    if label == "date":
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
        return metadata_tool(base, label, args)
    if label == "nc":
        require(args == ["-z", "-w", "5", "chatgpt.com", "443"], "unexpected egress probe")
    elif label == "df":
        print("Filesystem blocks used available mount\nfixture 524288 0 524288 /fixture")
    elif label == "pm" and args == ["list", "packages", "--show-versioncode", "org.portos.echoscreen"]:
        if (base / "apk-present").exists():
            print("package:org.portos.echoscreen versionCode:2")
    elif label == "pm" and args[:2] == ["install", "-r"]:
        require(Path(args[2]).is_file(), "installed APK absent")
        (base / "apk-present").touch()
    elif label == "pm" and args == ["grant", "org.portos.echoscreen", "android.permission.CAMERA"]:
        require((base / "apk-present").is_file(), "permission grant preceded APK installation")
        (base / "apk-camera-granted").touch()
    else:
        require(label == "am" and args == ["start", "-n", "org.portos.echoscreen/.MainActivity"], "Android call differs")
        require((base / "apk-camera-granted").is_file(), "activity launch preceded camera grant")
    return 0


def offline_update(box, release):
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
    manifest["files"]["bin/echo-bridge"] = sha(path.read_bytes())
    manifest_path.write_text(json.dumps(manifest))
    return bundle, manifest


def public_hashes(box, manifest):
    for name, expected in manifest["files"].items():
        if name.split("/", 1)[0] in ("bin", "apk", "scripts"):
            require(sha((box.device / name).read_bytes()) == expected, "installed public bytes differ")


def delivered_journey(binary, assets, archive, manifest):
    release = manifest["version"]
    update = "0.0.0-delivery" if release != "0.0.0-delivery" else "0.0.0-delivery.2"
    with DeliveryBox(binary, assets, archive, release) as box:
        with local_endpoint(box) as screen, local_endpoint(box) as health:
            (box.root / "ports.json").write_text(json.dumps({"tcp:16900": screen, "tcp:16901": health}))
            result = box.invoke("install", "--release-base", f"http://127.0.0.1:{health}")
            require(result["version"] == release, "default install version differs")
            require((box.root / "downloads").read_text().splitlines() == ["SHA256SUMS", archive.name],
                    "release asset requests differ")
            public_hashes(box, manifest)
            bundle, updated = offline_update(box, update)
            first_update_call = len(delivery_calls(box.root))
            require(box.invoke("update", "--from-dir", str(bundle))["version"] == update, "update version differs")
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
            require(box.invoke("doctor")["checks"] == [{"name": name, "ok": True, "fix": None} for name in
                    ("adb", "device", "hassmic", "egress", "clock", "disk_free", "process_user")],
                    "delivered doctor observation differs")
            box.invoke("rollback")
            public_hashes(box, manifest)
            require(box.invoke("status")["versions"] == {"current": release, "previous": None},
                    "rollback did not restore previous release")
            require(not any((box.root / "remote/tmp").iterdir()), "staging was retained")
            require(json.loads((box.root / "forwards.json").read_text()) == {}, "owned forwards were retained")
            calls = [call[-1] for call in delivery_calls(box.root)]
            for primitive in ("prepare-archive", "--device install --bundle", "--device screen-setup",
                              "--device start", "--device rollback"):
                require(any(primitive in call for call in calls), "shell dispatch missing " + primitive)
            box.invoke("uninstall")
    print("F33 delivered native install/setup/update/status/doctor/rollback: PASS")


if __name__ == "__main__":
    require(sys.argv[1:2] == ["--fake"], "only a test boundary entry point")
    base = Path(os.environ["FAKE_ROOT"])
    label, *args = sys.argv[2:]
    if label == "adb":
        sys.exit(recording_adb(base, args))
    sys.exit(fake_tool(base, label, args))
