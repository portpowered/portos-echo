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
from unittest.mock import patch

from test_echo_install import Sandbox, fake_adb, require, SENTINEL
from test_echo_install_lifecycle import configure_lifecycle, release_started, release_stop

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
        configure_lifecycle(self, bridge=self.root / "synthetic-bridge")
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
            with tarfile.open(box.archive) as bundle:
                require((box.device / "bin/echo-bridge").read_bytes() == bundle.extractfile("bin/echo-bridge").read(),
                        "bridge bytes differ from packaged input")
            release_started(box)
            install = next(i for i, c in enumerate(calls) if c[:2] == ["install", "-r"])
            start = next(i for i, c in enumerate(calls) if "setsid /data/local/hassmic/runas" in " ".join(c))
            require(start < install and sum(c[:2] == ["install", "-r"] for c in calls) == 1, "screen order/count differs")
            downloads = [json.loads(line) for line in (box.root / "downloads").read_text().splitlines()]
            require([c[-1] for c in downloads] == [f"https://github.com/portpowered/portos-echo/releases/download/v{box.version}/{name}" for name in (box.archive.name, "SHA256SUMS")], "version/download mismatch")
            require(all(("--https-only" in c if wget else "--proto-redir" in c) for c in downloads), "HTTPS restriction missing")
            release_stop(box)
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
                               ("start", "start failed"), ("prepare", "start failed"),
                               ("ownership", "start failed"), ("spawn", "start failed"), ("screen", "bridge already started"),
                               ("home", "bridge already started"), ("launch", "bridge already started")):
        with ReleaseBox() as box:
            if fault == "hassmic":
                (box.root / "remote/hassmic/runas").unlink()
            elif fault in ("install", "prepare", "ownership", "spawn"):
                point = {"install": "push", "prepare": "mkfifo", "ownership": "chown -R '9999:9999'", "spawn": "setsid"}[fault]
                (box.root / "fault").write_text(point)
            elif fault == "start":
                (box.root / "mode").write_text("stale")
            else:
                (box.root / (fault + "-fails")).touch()
            try: result = box.invoke(code=1)
            except subprocess.TimeoutExpired as error: raise AssertionError("release failure timed out: " + fault) from error
            require(diagnostic in result.stderr, "failure diagnostic differs")
            if fault == "start": require((box.root / "stale-observed").exists(), "stale case skipped observation")
            if fault in ("hassmic", "install", "start", "prepare", "ownership", "spawn"):
                require(not any(c[:2] == ["install", "-r"] for c in box.calls()), "failure continued to screen")
            if fault in ("prepare", "ownership", "spawn", "start"):
                require((box.device / "bin/echo-bridge").is_file(), "start failure lost installed files")
                require(not (box.device / "private/run").exists(), "start failure leaked run state")
                require(not (box.root / "remote/echo-bridge.lock").exists(), "start failure leaked lock")
            if fault in ("prepare", "ownership"):
                require(not any("setsid " in " ".join(c) for c in box.calls()), "preparation failure launched")
            if fault in ("screen", "home", "launch"):
                release_started(box)
                release_stop(box)
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
        release_started(box)
        release_stop(box)
        override.write_text("replacement")
        box.invoke(["--config", str(override)])
        release_started(box, starts=2)
        require(prompt.read_text() == "edited voice", "prompt edit lost")
        require((box.device / "private/bridge.toml").read_text() == SENTINEL, "existing config lost")
        require(sum(c[:2] == ["install", "-r"] for c in box.calls()) == 1, "existing APK reinstalled")
        require(not (box.root / "escaped").exists(), "filename executed")
        release_stop(box)
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
    path_diagnostic_cases()
    native_flags_cases()
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
            if name == "licenses/extra.txt":
                result = subprocess.run([sys.executable, str(ROOT / "scripts/package-dist.py"),
                                         "verify", str(box.archive), "--checksums",
                                         str(box.assets / "SHA256SUMS")], capture_output=True, text=True)
                require(result.returncode == 1 and result.stdout == "", "privacy CLI status differs")
                require(result.stderr == "release-kit: private path or key in licenses/extra.txt\n",
                        "privacy diagnostic must identify the public member without its content or host path")
    print("U-PACK I-BUNDLE: PASS")


def path_diagnostic_cases():
    paths = (b'/home/example/.cargo/registry/src/index/public-1.0/src/lib.rs',
             b'/Users/example/.rustup/toolchains/stable/lib/source.rs',
             b'C:\\Users\\example\\private\\file')
    counts = PACK.private_path_counts(b'\x00' + b'\x00'.join(paths) + b'\x00' + paths[0])
    require(counts == {"registry": 2, "toolchain": 1, "checkout": 0, "other": 1,
                       "current_home": 0, "current_cargo": 0, "current_rustup": 0, "external": 4},
            "sanitized path category counts differ")
    require(all(count == 0 for count in PACK.private_path_counts(b'/data/local/echo/home/prompts').values()),
            "device home classified as a host path")
    for boundary in (b'\v', b'\f', b'='):
        require(PACK.private_path_counts(boundary + paths[0])["registry"] == 1,
                "path delimiter changed classification")
    with patch.object(PACK.Path, "home", return_value=Path("/home/example")), \
            patch.object(PACK, "ROOT", Path("/home/example/source")):
        flags = b'-ffile-prefix-map=/home/example=/build/user -ffile-prefix-map=/home/example/source=/build/echo\x00'
        mapped = PACK.private_path_counts(flags * 2)
        require(mapped["checkout"] == 2 and mapped["other"] == 2 and mapped["current_home"] == 4
                and mapped["external"] == 0, "remap arguments misclassified as external paths")
    with tempfile.TemporaryDirectory() as temporary:
        assets = Path(temporary)
        codex = assets / "codex"
        codex.mkdir()
        for name in ("codex-app-server", "cacert.pem", "LICENSE", "NOTICE"):
            (codex / name).write_bytes(b"synthetic public input")
        (codex / "codex-app-server").write_bytes(b'\x00'.join(paths))
        try:
            PACK.assemble(assets, "0.1.0", "a" * 40, "b" * 40, codex,
                          codex / "LICENSE", codex / "NOTICE",
                          {"licenses/ca/MPL-2.0.txt": b"license", "licenses/ca/NOTICE": b"notice"})
            raise AssertionError("private binary accepted")
        except PACK.BundleError:
            pass
        result = subprocess.run([sys.executable, str(ROOT / "scripts/package-dist.py"), "verify",
                                 str(assets / "echo-0.1.0-armv7.tar.gz"), "--checksums",
                                 str(assets / "SHA256SUMS")], capture_output=True, text=True)
        require(result.returncode == 1 and result.stdout == "", "private binary CLI status differs")
        expected = dict(counts, registry=1, external=3)
        require(result.stderr == "release-kit: rejected path categories " + json.dumps(expected, sort_keys=True)
                + "\nrelease-kit: private path or key in bin/codex-app-server\n", "diagnostic leaked or counts differ")
    print("U-PACK sanitized path diagnostics: PASS")


def native_metadata_case(work):
    caller = {"CFLAGS": "-DRELEASE_MARKER=7", "CXXFLAGS": "-DRELEASE_MARKER=8",
              "CARGO_ENCODED_RUSTFLAGS": "-Copt-level=2"}
    with patch.dict(os.environ, caller):
        env = PACK.build_environment(work)
    require(Path(env["CARGO_HOME"]) == work / "cargo-home", "Cargo home escaped workspace")
    require(Path(env["CARGO_HOME"]).is_dir(), "private Cargo home missing")
    for key in ("CFLAGS", "CXXFLAGS"):
        require(env[key] == caller[key], "native caller flags changed or response argument injected")
        require(PACK.PRIVATE.search(("compiler: cc " + env[key]).encode()) is None,
                "native build metadata exposes a private path")
    require(env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") == ["-Copt-level=2",
            "--remap-path-prefix=" + str(Path.home()) + "=/build/user",
            "--remap-path-prefix=" + str(ROOT) + "=/build/echo"], "Rust maps or caller flags lost")
    require(not (work / "native-remap.rsp").exists(), "native response file still generated")
    return env


def native_flags_cases():
    with tempfile.TemporaryDirectory(prefix="release-native-", dir="/tmp") as temporary:
        work = Path(temporary)
        env = native_metadata_case(work)
        require(Path(env["CARGO_HOME"]).stat().st_mode & 0o777 == 0o700, "Cargo home not private")
        probe = Path(env["CARGO_HOME"]) / "probe.c"
        probe.write_text("RELEASE_MARKER\n__FILE__\n")
        for tool, key, marker in (("cc", "CFLAGS", "7"), ("c++", "CXXFLAGS", "8")):
            result = subprocess.run([tool, *shlex.split(env[key]), "-E", "-P", str(probe)],
                                    capture_output=True, text=True, timeout=20)
            require(result.returncode == 0, "native preprocessing failed")
            require(result.stdout.splitlines() == [marker, json.dumps(str(probe))],
                    "native macro value or dependency source location differs")
            require(PACK.PRIVATE.search(result.stdout.encode()) is None, "native source path is private")
    print("U-PACK private Cargo sources and native metadata: PASS")


def configure_native():
    """Explicit network provisioning, then configure only; never compiles Codex."""
    import tomllib
    import urllib.request
    helper = (ROOT / "scripts/build-codex-armv7.sh").read_text()
    pin, = re.findall(r"(?m)^  commit=([0-9a-f]{40})$", helper)
    lock_url = "https://raw.githubusercontent.com/openai/codex/" + pin + "/codex-rs/Cargo.lock"
    with urllib.request.urlopen(lock_url, timeout=60) as response:
        lock = tomllib.loads(response.read().decode())
    package, = (p for p in lock["package"] if p["name"] == "openssl-src")
    name = "openssl-src-" + package["version"]
    with urllib.request.urlopen("https://static.crates.io/crates/openssl-src/" + name + ".crate", timeout=60) as response:
        data = response.read()
    require(sha(data) == package["checksum"], "OpenSSL source checksum differs from pinned lock")
    with tempfile.TemporaryDirectory(prefix="release-configure-", dir="/tmp") as temporary:
        work = Path(temporary)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            archive.extractall(work, filter="data")
        env = PACK.build_environment(work)
        tools = Path(env["ANDROID_NDK_HOME"]) / "toolchains/llvm/prebuilt/linux-x86_64/bin"
        env.update(CC=str(tools / "armv7a-linux-androideabi24-clang"), AR=str(tools / "llvm-ar"),
                   RANLIB=str(tools / "llvm-ranlib"))
        env.pop("CROSS_COMPILE", None)
        command = ["perl", "./Configure", "--prefix=" + str(work / "install"),
                   "--openssldir=/usr/local/ssl", "no-shared", "no-module", "no-tests", "no-comp",
                   "no-zlib", "no-zlib-dynamic", "--libdir=lib", "no-ssl3", "no-md2", "no-rc5",
                   "no-weak-ssl-ciphers", "no-camellia", "no-idea", "no-seed", "no-stdio", "linux-armv4",
                   "-O2", "-DANDROID", "-ffunction-sections", "-fdata-sections", "-fPIC", "-g",
                   "-gdwarf-2", "-fno-omit-frame-pointer", "-w", *shlex.split(env.get("CFLAGS", ""))]
        source = work / name / "openssl"
        failed = subprocess.run([*command, "@" + str(work / "native-remap.rsp")], cwd=source,
                                env=env, capture_output=True, text=True, timeout=60)
        require(failed.returncode != 0 and "target already defined" in failed.stderr,
                "previous OpenSSL Configure failure not reproduced")
        passed = subprocess.run(command, cwd=source, env=env, capture_output=True, timeout=60)
        require(passed.returncode == 0, "corrected OpenSSL Configure failed")
    print("OpenSSL Configure: previous_response=REJECTED private_cargo_no_response=PASS")


def main():
    global ASSETS
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--configure-native", action="store_true")
    args = parser.parse_args()
    require(sys.platform == "linux", "Linux is required for this runtime witness")
    if args.configure_native:
        configure_native()
        return
    ASSETS = args.assets.resolve() if args.assets else None
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda function: function(), (package_cases, happy_cases, integrity_cases, failure_cases, retention_cases, interruption_case)))
    from test_release_redirects import redirect_cases
    redirect_cases(ReleaseBox)
    if ASSETS:
        independent_bundle(ASSETS)
        print("release-kit: bundle=PASS manifest=PASS privacy=PASS")
    print("release-kit tests: integrity=PASS install=PASS failures=PASS retention=PASS privacy=PASS")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fake"]:
        sys.exit(fake_boundary())
    main()
