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
import struct
import subprocess
import sys
import tarfile
import tempfile
from unittest.mock import patch

from test_echo_install import Sandbox, fake_adb, require, SENTINEL

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("package_dist", ROOT / "scripts/package-dist.py")
PACK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACK)


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
    if kind == "installer":
        (base / "installer-args.json").write_text(json.dumps(args))
        print("synthetic installer executed")
        status = base / "installer-exit"
        if status.exists():
            print("synthetic installer needs resume", file=sys.stderr)
            return int(status.read_text())
        return 0
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
        self.version = "0.1.0"
        self.archive = self.assets / f"echo-{self.version}-armv7.tar.gz"
        self.installer = self.assets / f"echo-setup-{self.version}-linux-x86_64"
        self.wrapper = self.assets / "install.sh"
        payload = self.root / "installer"
        entry = shlex.quote(sys.executable) + " " + shlex.quote(str(Path(__file__).resolve())) + " --fake "
        executable(payload, 'python3 "$ECHO_SETUP_TEST_DRIVER" --fake installer')
        self.env["ECHO_SETUP_TEST_DRIVER"] = str(Path(__file__).resolve())
        assemble_fixture(self.assets, self.files["bridge"], self.files["ca"], (payload, payload))
        for name in ("sha256sum", "chmod", "python3"):
            executable(self.tools / name, shlex.quote(shutil.which(name)))
        executable(self.tools / ("wget" if wget else "curl"), entry + ("wget" if wget else "curl"))
        self.env["TMPDIR"] = str(self.root / "tmp")
        (self.root / "tmp").mkdir()

    def invoke(self, args=(), code=0, pipe=False):
        command = ["/bin/sh", *(["-s", "--"] if pipe else [str(self.wrapper)]), *args]
        result = subprocess.run(command, input=self.wrapper.read_text() if pipe else None,
                                env=self.env, cwd=self.root, capture_output=True, text=True, timeout=20)
        require(result.returncode == code, f"wrapper expected {code}, got {result.returncode}: " + result.stderr)
        require(SENTINEL not in result.stdout + result.stderr, "private diagnostic leaked")
        require(str(self.root) not in result.stdout + result.stderr, "local path leaked")
        require(not list((self.root / "tmp").iterdir()), "private temporary data leaked")
        return result

    def installer_args(self):
        ledger = self.root / "installer-args.json"
        return json.loads(ledger.read_text()) if ledger.exists() else None

    def rehash(self):
        (self.assets / "SHA256SUMS").write_text("".join(
            f"{sha((self.assets / name).read_bytes())}  {name}\n" for name in asset_names(self.version)))


def asset_names(version):
    return (f"echo-{version}-armv7.tar.gz", f"echo-setup-{version}-linux-x86_64",
            f"echo-setup-{version}-windows-x86_64.exe", "install.sh", "install.ps1")


def assemble_fixture(assets, bridge, apk, installers, private_binary=None):
    codex = assets.parent / "codex"
    codex.mkdir()
    for name in ("codex-app-server", "cacert.pem", "LICENSE", "NOTICE"):
        (codex / name).write_text("synthetic public " + name)
    if private_binary:
        (codex / "codex-app-server").write_bytes(private_binary)
    PACK.assemble(assets, "0.1.0", "a" * 40, "b" * 40, codex, bridge, apk,
                  {"licenses/ca/MPL-2.0.txt": b"synthetic license", "licenses/ca/NOTICE": b"CA attribution"},
                  installers)


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
    lines = (assets / "SHA256SUMS").read_text().splitlines()
    expected = {sha((assets / name).read_bytes()) + "  " + name for name in asset_names(manifest["version"])}
    require(len(lines) == len(expected) and set(lines) == expected, "asset hashes differ")
    require(manifest["components"]["ca_bundle"]["sha256"] == sha(files["bin/cacert.pem"]), "CA provenance differs")
    PACK.verify(archive, assets / "SHA256SUMS")
    return manifest


def verify_delivered_assets(assets):
    manifest = independent_bundle(assets)
    release = manifest["version"]
    _, linux_name, windows_name, *_ = asset_names(release)
    require({path.name for path in assets.iterdir()} == {*asset_names(release), "SHA256SUMS"},
            "release asset inventory differs")
    linux = assets / linux_name
    # Artifact archives do not necessarily preserve executable permission.
    with tempfile.TemporaryDirectory(prefix="release-setup-") as directory:
        executable = Path(directory) / linux_name
        shutil.copyfile(linux, executable)
        executable.chmod(0o700)
        header = linux.read_bytes()[:64]
        require(header[:6] == b"\x7fELF\x02\x01" and header[18:20] == b"\x3e\x00", "installer is not x86_64 ELF")
        pe = (assets / windows_name).read_bytes()
        require(pe.startswith(b"MZ") and len(pe) >= 64, "installer missing DOS header")
        address, = struct.unpack_from("<I", pe, 60)
        require(pe[address:address + 6] == b"PE\x00\x00\x64\x86", "installer is not x86_64 PE")
        result = subprocess.run([str(executable), "--version"], capture_output=True, text=True, timeout=5)
        require((result.returncode, result.stdout, result.stderr) == (0, f"echo-setup {release}\n", ""),
                "embedded installer version differs")
    print("release-kit: manifest=PASS checksums=PASS ELF-x86_64=PASS PE-x86_64=PASS embedded-version=PASS")


def happy_cases():
    for wget, pipe, child_exit in ((False, False, 0), (True, False, 0), (False, True, 3)):
        with ReleaseBox(wget) as box:
            config = box.root / "config ' $(touch escaped).toml"
            config.write_text(SENTINEL)
            if child_exit:
                (box.root / "installer-exit").write_text(str(child_exit))
            result = box.invoke(["--config", str(config)], code=child_exit, pipe=pipe)
            require(result.stdout == "synthetic installer executed\n", "installer output differs")
            require(result.stderr == ("synthetic installer needs resume\n" if child_exit else ""),
                    "installer stderr differs")
            require(box.installer_args() == ["install", "--version", box.version, "--config", str(config)],
                    "installer arguments differ")
            require(box.calls() == [] and not (box.root / "escaped").exists(), "unexpected bootstrap effect")
            downloads = [json.loads(line) for line in (box.root / "downloads").read_text().splitlines()]
            base = f"https://github.com/portpowered/portos-echo/releases/download/v{box.version}"
            require([c[-1] for c in downloads] == [f"{base}/{name}" for name in (box.installer.name, "SHA256SUMS")],
                    "version/download mismatch")
    print("F28 Linux bootstrap: verified child, exact argv, cleanup and exit propagation=PASS")


def integrity_cases():
    for fault in ("tamper", "missing", "duplicate", "malformed", "download"):
        with ReleaseBox() as box:
            sums = box.assets / "SHA256SUMS"
            if fault == "tamper":
                with box.installer.open("ab") as binary:
                    binary.write(b"changed")
            elif fault == "missing":
                sums.write_text("")
            elif fault == "duplicate":
                sums.write_text(sums.read_text() * 2)
            elif fault == "malformed":
                sums.write_text("invalid  " + box.installer.name + "\n")
            else:
                (box.root / "download-fails").touch()
            result = box.invoke(code=1)
            expected = {"tamper": "installer checksum mismatch", "download": "download failed"}
            require(result.stderr == "echo-release: " + expected.get(fault, "invalid installer checksum") + "\n",
                    "integrity failure diagnostic differs")
            require(result.stdout == "" and box.installer_args() is None and box.calls() == [],
                    "integrity/download failure ran installer or adb")
    print("F30 F32 Linux bootstrap: integrity/download installer_exec_count=0 adb=0")


def usage_cases():
    for args, code in ((["--version", "$(touch escaped)"], 2), (["--config", "missing"], 1),
                        (["--version", "0.1.0\ninvalid"], 2), (["--unknown"], 2), ([], 2)):
        with ReleaseBox() as box:
            if not args:
                box.wrapper.write_bytes((ROOT / "scripts/install.sh").read_bytes())
            box.invoke(args, code=code)
            require(box.installer_args() is None and box.calls() == [] and not (box.root / "downloads").exists(),
                    "invalid CLI caused effects")
    print("F32 Linux bootstrap: source version and usage refusals=PASS")


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
            require(child.returncode != 0 and b"synthetic installer executed" not in stdout, "interruption reported success")
            require(SENTINEL.encode() not in stdout + stderr, "interruption leaked private data")
            require(box.installer_args() is None and box.calls() == [] and not list((box.root / "tmp").iterdir()),
                    "interruption leaked state")
        finally:
            os.close(fd)
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.communicate()
    print("F32 Linux bootstrap: interruption cleanup=PASS")


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
        checksum_set_cases(box)
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


def checksum_set_cases(box):
    sums = box.assets / "SHA256SUMS"
    rows = sums.read_text().splitlines()
    for invalid in (rows[:-1], [*rows, rows[0]], [*rows, "0" * 64 + "  unexpected"],
                    [*rows[:-1], rows[-1].replace("  ", " ", 1)]):
        sums.write_text("\n".join(invalid) + "\n")
        try:
            PACK.verify(box.archive, sums)
        except PACK.BundleError:
            pass
        else:
            raise AssertionError("invalid asset checksum set accepted")


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
        assets = Path(temporary) / "assets"
        assets.mkdir()
        fixture = assets.parent / "public"
        fixture.write_bytes(b"synthetic public input")
        try:
            assemble_fixture(assets, fixture, fixture, (fixture, fixture), b'\x00'.join(paths))
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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--configure-native", action="store_true")
    args = parser.parse_args()
    require(sys.platform == "linux", "Linux is required for this runtime witness")
    if args.configure_native:
        configure_native()
        return
    if args.assets:
        verify_delivered_assets(args.assets.resolve())
        return
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda function: function(), (package_cases, happy_cases, integrity_cases, usage_cases, interruption_case)))
    print("release-kit tests: integrity=PASS bootstrap=PASS privacy=PASS")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fake"]:
        sys.exit(fake_boundary())
    main()
