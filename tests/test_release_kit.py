"""T1/T2 packaging and T3 shell journeys; closed PATH, synthetic adb only."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import signal
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile

from test_echo_install import Sandbox, fake_adb, require, SENTINEL

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("package_dist", ROOT / "scripts/package-dist.py")
PACK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACK)
ASSETS = None


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fake_boundary():
    base = Path(os.environ["FAKE_ROOT"])
    kind, *args = sys.argv[2:]
    if kind in ("curl", "wget"):
        with (base / "downloads").open("a") as output:
            print(json.dumps([kind, *args]), file=output)
        if (base / "download-fails").exists():
            print(SENTINEL, file=sys.stderr)
            return 1
        if (base / "block-download").exists():
            with (base / "download-ready").open("w") as ready:
                ready.write("ready")
            with (base / "download-release").open("r") as release:
                release.read()
        destination = args[args.index("--output") + 1] if kind == "curl" else next(a.split("=", 1)[1] for a in args if a.startswith("--output-document="))
        url = args[-1]
        require(url.startswith("https://github.com/portpowered/portos-echo/releases/download/v"), "unexpected download")
        shutil.copyfile(base / "assets" / url.rsplit("/", 1)[1], destination)
        return 0
    intercepted = False
    code = 0
    if args[:3] == ["shell", "pm", "list"]:
        if (base / "screen-present").exists():
            print("package:org.portos.echoscreen")
        intercepted = True
    elif args[:2] == ["install", "-r"]:
        code = 1 if (base / "screen-fails").exists() else 0
        if code == 0:
            (base / "screen-present").touch()
        intercepted = True
    elif args[:2] == ["shell", "am"]:
        intercepted = True
        code = 1 if (base / "launch-fails").exists() else 0
    elif args[0] == "shell" and len(args) == 2:
        command = args[1]
        if "screen_home" in command:
            intercepted = True
            code = 1 if (base / "home-fails").exists() else 0
        elif "setsid /system/bin/sh" in command:
            intercepted = True
            (base / "running").touch()
        elif "read -r end <" in command:
            intercepted = True
            code = 1 if (base / "start-fails").exists() else 0
        elif "test -x '/data/local/echo-bridge/private/launch.sh'" in command:
            # Fake Android start boundary: no ARM binary is executed.
            intercepted = True
        elif "echo-bridge/private/run/control" in command and "while ! test" in command:
            intercepted = True
            (base / "running").unlink(missing_ok=True)
    if intercepted:
        with (base / "ledger").open("a") as output:
            print(json.dumps(args), file=output)
        print(SENTINEL, file=sys.stderr)
        return code
    sys.argv = [sys.argv[0], "--fake-adb", *args]
    return fake_adb()


def executable(path, command):
    path.write_text("#!/bin/sh\nexec " + command + ' "$@"\n')
    path.chmod(0o755)


class ReleaseBox(Sandbox):
    def __init__(self, wget=False):
        super().__init__()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        if ASSETS:
            for path in ASSETS.iterdir():
                if path.is_file():
                    shutil.copyfile(path, self.assets / path.name)
        else:
            codex = self.root / "codex"
            codex.mkdir()
            for name in ("codex-app-server", "cacert.pem", "LICENSE", "NOTICE"):
                (codex / name).write_text("synthetic public " + name)
            PACK.assemble(self.assets, "0.1.0", "a" * 40, "b" * 40, codex, self.files["bridge"], self.files["ca"],
                          {"licenses/ca/MPL-2.0.txt": b"synthetic license", "licenses/ca/NOTICE": b"CA attribution"})
        self.archive = next(self.assets.glob("echo-*-armv7.tar.gz"))
        self.version = self.archive.name.removeprefix("echo-").removesuffix("-armv7.tar.gz")
        self.wrapper = self.assets / "install.sh"
        for name in ("sh", "tar", "gzip", "sha256sum", "mkdir"):
            executable(self.tools / name, shlex.quote(shutil.which(name)))
        entry = shlex.quote(sys.executable) + " " + shlex.quote(str(Path(__file__).resolve())) + " --fake "
        for name in ("adb", "wget" if wget else "curl"):
            executable(self.tools / name, entry + name)
        self.env["TMPDIR"] = str(self.root / "tmp")
        (self.root / "tmp").mkdir()
        (self.root / "remote/hassmic/boot.log").write_text("old client connected\n")

    def invoke(self, args=(), code=0, pipe=False):
        command = ["/bin/sh", *([] if pipe else [str(self.wrapper)]), *args]
        result = subprocess.run(command, input=self.wrapper.read_text() if pipe else None,
                                env=self.env, cwd=self.root, capture_output=True, text=True, timeout=20)
        require(result.returncode == code, f"wrapper expected {code}, got {result.returncode}: " + result.stderr)
        require(SENTINEL not in result.stdout + result.stderr, "private diagnostic leaked")
        require(str(self.root) not in result.stdout + result.stderr, "local path leaked")
        require(not list((self.root / "tmp").iterdir()), "private temporary data leaked")
        if code:
            require("Echo installed and started" not in result.stdout, "false success")
        return result

    def rehash(self):
        (self.assets / "SHA256SUMS").write_text(f"{sha(self.archive.read_bytes())}  {self.archive.name}\n{sha(self.wrapper.read_bytes())}  install.sh\n")


def independent_bundle(assets):
    archive = next(assets.glob("echo-*-armv7.tar.gz"))
    with tarfile.open(archive) as bundle:
        members = bundle.getmembers()
        require(all(m.isfile() for m in members), "non-file payload")
        files = {m.name: bundle.extractfile(m).read() for m in members}
        require(len(files) == len(members), "duplicate member")
    manifest = json.loads(files.pop("MANIFEST"))
    require(set(files) == set(manifest["files"]), "manifest membership differs")
    for name, content in files.items():
        require(sha(content) == manifest["files"][name], "payload hash differs")
        require(PACK.safe_name(name), "forbidden member")
    hashes = dict((name, value) for value, name in (line.split() for line in (assets / "SHA256SUMS").read_text().splitlines()))
    require(hashes == {archive.name: sha(archive.read_bytes()), "install.sh": sha((assets / "install.sh").read_bytes())}, "asset hashes differ")
    require(manifest["components"]["ca_bundle"]["sha256"] == sha(files["bin/cacert.pem"]), "CA provenance differs")
    PACK.verify(archive, assets / "SHA256SUMS")


def happy_cases():
    for wget, pipe in ((False, False), (True, False), (False, True)):
        with ReleaseBox(wget) as box:
            independent_bundle(box.assets)
            result = box.invoke(pipe=pipe)
            require("ChatGPT sign-in" in result.stdout and "Port OS link" in result.stdout, "next steps absent")
            calls = box.calls()
            pushes = [c for c in calls if c[0] == "push"]
            require(len(pushes) == 6, "required payload not installed")
            require((box.device / "bin/echo-bridge").read_bytes() != b"", "bridge not copied")
            install = next(i for i, c in enumerate(calls) if c[:2] == ["install", "-r"])
            start = next(i for i, c in enumerate(calls) if "setsid /system/bin/sh" in " ".join(c))
            require(start < install and sum(c[:2] == ["install", "-r"] for c in calls) == 1, "screen order/count differs")
            downloads = [json.loads(line) for line in (box.root / "downloads").read_text().splitlines()]
            require([c[-1] for c in downloads] == [f"https://github.com/portpowered/portos-echo/releases/download/v{box.version}/{name}" for name in (box.archive.name, "SHA256SUMS")], "version/download mismatch")
            require(all(("--https-only" in c if wget else "--proto-redir" in c) for c in downloads), "HTTPS restriction missing")
    print("F01 F03 F16 F17: PASS")


def integrity_cases():
    for fault in ("tamper", "missing", "duplicate", "malformed", "download"):
        with ReleaseBox() as box:
            sums = box.assets / "SHA256SUMS"
            if fault == "tamper":
                box.archive.write_bytes(box.archive.read_bytes() + b"changed")
            elif fault == "missing":
                sums.write_text("")
            elif fault == "duplicate":
                sums.write_text(sums.read_text() * 2)
            elif fault == "malformed":
                sums.write_text("invalid  " + box.archive.name + "\n")
            else:
                (box.root / "download-fails").touch()
            box.invoke(code=1)
            require(box.calls() == [], "integrity/download failure touched adb")
    for name, kind in (("../outside", tarfile.REGTYPE), ("/absolute", tarfile.REGTYPE),
                       ("bin/echo-bridge", tarfile.SYMTYPE), ("bin/echo-bridge", tarfile.FIFOTYPE)):
        with ReleaseBox() as box:
            with tarfile.open(box.archive, "w:gz") as bundle:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.linkname = "outside" if kind == tarfile.SYMTYPE else ""
                bundle.addfile(member, io.BytesIO())
            box.rehash()
            result = box.invoke(code=1)
            require("unsafe archive" in result.stderr and box.calls() == [], "unsafe archive accepted")
    print("F04 F05 F06 F07: PASS")


def failure_cases():
    for fault, diagnostic in (("hassmic", "hassmic must"), ("install", "install failed"),
                               ("start", "start failed"), ("screen", "bridge already started"),
                               ("home", "bridge already started"), ("launch", "bridge already started")):
        with ReleaseBox() as box:
            if fault == "hassmic":
                (box.root / "remote/hassmic/runas").unlink()
            elif fault == "install":
                (box.root / "fault").write_text("push")
            else:
                (box.root / (fault + "-fails")).touch()
            result = box.invoke(code=1)
            require(diagnostic in result.stderr, "failure diagnostic differs")
            if fault in ("hassmic", "install", "start"):
                require(not any(c[:2] == ["install", "-r"] for c in box.calls()), "failure continued to screen")
            if fault == "hassmic":
                require(not any(c[0] == "push" for c in box.calls()), "absent hassmic mutated state")
    for devices in ("", "fixture\toffline\n", "a\tdevice\nb\tdevice\n"):
        with ReleaseBox() as box:
            (box.root / "devices").write_text(devices)
            box.invoke(code=1)
            require(box.calls() == [["devices"]], "bad selection caused effects")
    with ReleaseBox() as box:
        (box.tools / "adb").unlink()
        box.invoke(code=1)
        require(not (box.root / "downloads").exists(), "missing adb downloaded")
    print("F08 F09 F10 F11 F12: PASS")


def retention_cases():
    with ReleaseBox() as box:
        override = box.root / "config ' $(touch escaped).toml"
        override.write_text(SENTINEL)
        box.invoke(["--version", "v" + box.version, "--config", str(override)])
        require((box.device / "private/bridge.toml").read_text() == SENTINEL, "config bytes changed")
        prompt = box.device / "home/prompts/voice.md"
        prompt.write_text("edited voice")
        (box.root / "running").unlink()
        override.write_text("replacement")
        box.invoke(["--config", str(override)])
        require(prompt.read_text() == "edited voice", "prompt edit lost")
        require((box.device / "private/bridge.toml").read_text() == SENTINEL, "existing config lost")
        require(sum(c[:2] == ["install", "-r"] for c in box.calls()) == 1, "existing APK reinstalled")
        require(not (box.root / "escaped").exists(), "filename executed")
    with ReleaseBox() as box:
        (box.root / "remote/echo-bridge.lock").mkdir()
        box.invoke(code=1)
        require((box.root / "remote/echo-bridge.lock").is_dir() and not box.device.exists(), "competing install mutated state")
    for args, code in ((["--version", "$(touch escaped)"], 2), (["--config", "missing"], 1),
                        (["--version", "0.1.0\ninvalid"], 2), (["--unknown"], 2)):
        with ReleaseBox() as box:
            box.invoke(args, code=code)
            require(box.calls() == [] and not (box.root / "downloads").exists(), "invalid CLI caused effects")
    print("F02 F13 F14 F15: PASS")


def interruption_case():
    import selectors
    with ReleaseBox() as box:
        (box.root / "block-download").touch()
        os.mkfifo(box.root / "download-ready")
        os.mkfifo(box.root / "download-release")
        fd = os.open(box.root / "download-ready", os.O_RDWR | os.O_NONBLOCK)
        child = subprocess.Popen(["/bin/sh", str(box.wrapper)], env=box.env, cwd=box.root,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(fd, selectors.EVENT_READ)
                require(bool(selector.select(10)), "download barrier not reached")
            os.read(fd, 100)
            os.killpg(child.pid, signal.SIGTERM)
            stdout, stderr = child.communicate(timeout=10)
            require(child.returncode != 0 and b"Echo installed" not in stdout, "interruption reported success")
            require(SENTINEL.encode() not in stdout + stderr, "interruption leaked private data")
            require(box.calls() == [] and not list((box.root / "tmp").iterdir()), "interruption leaked state")
        finally:
            os.close(fd)
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.communicate()
    print("F14 cancellation: PASS")


def package_cases():
    for value in ("0.1.0", "0.0.0-dev.abc123"):
        require(PACK.version(value) == value, "version changed")
    for value in ("v0.1.0", "../escape", "0.1.0\n", "0.1.0;true"):
        try:
            PACK.version(value)
            raise AssertionError("invalid version accepted")
        except ValueError:
            pass
    with ReleaseBox() as box:
        independent_bundle(box.assets)
        for marker in (b"/home/example/private/file", b"C:\\Users\\example\\private", b"-----BEGIN PRIVATE KEY-----"):
            require(PACK.PRIVATE.search(marker) is not None, "private marker missed")
        for name in ("../escape", "licenses/token.txt", "credentials.json", "/absolute", "licenses/../escape"):
            require(not PACK.safe_name(name), "sensitive member accepted")
        with tarfile.open(box.archive) as bundle:
            projected = bundle.extractfile("examples/bridge.echo.toml").read()
        parsed = PACK.tomllib.loads(projected.decode())
        original = PACK.tomllib.loads((ROOT / "examples/bridge.echo.toml").read_text())
        original["codex"]["binary"] = "/data/local/echo-bridge/bin/codex-app-server"
        original["codex"]["ca_file"] = "/data/local/echo-bridge/bin/cacert.pem"
        require(parsed == original, "projection changed unrelated config")
    for name, content, update in (("licenses/extra.txt", b"/home/example/private/file", True),
                                  ("licenses/secret.txt", b"seed", True),
                                  ("bin/cacert.pem", b"changed", False)):
        with ReleaseBox() as box:
            with tarfile.open(box.archive) as bundle:
                files = {m.name: bundle.extractfile(m).read() for m in bundle.getmembers()}
            files[name] = content
            if update:
                manifest = json.loads(files["MANIFEST"])
                manifest["files"][name] = sha(content)
                files["MANIFEST"] = json.dumps(manifest).encode()
            with tarfile.open(box.archive, "w:gz") as bundle:
                for leaf, data in files.items():
                    member = tarfile.TarInfo(leaf)
                    member.size = len(data)
                    member.mode = 0o644
                    bundle.addfile(member, io.BytesIO(data))
            box.rehash()
            try:
                PACK.verify(box.archive, box.assets / "SHA256SUMS")
                raise AssertionError("seeded integrity/privacy violation accepted")
            except ValueError:
                pass
    print("U-PACK I-BUNDLE: PASS")


def main():
    global ASSETS
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path)
    args = parser.parse_args()
    require(sys.platform == "linux", "Linux is required for this runtime witness")
    ASSETS = args.assets.resolve() if args.assets else None
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda function: function(), (package_cases, happy_cases, integrity_cases, failure_cases, retention_cases, interruption_case)))
    if ASSETS:
        independent_bundle(ASSETS)
        print("release-kit: bundle=PASS manifest=PASS privacy=PASS")
    print("release-kit tests: integrity=PASS install=PASS failures=PASS retention=PASS privacy=PASS")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fake"]:
        sys.exit(fake_boundary())
    main()
