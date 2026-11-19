"""Build and independently read back the explicit Linux release payload."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
VERSION = r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*)?"
REQUIRED = {
    "bin/echo-bridge", "bin/codex-app-server", "bin/cacert.pem", "apk/echo-screen.apk",
    "scripts/echo-install.sh", "examples/bridge.echo.toml", "examples/prompts/voice.md",
    "examples/prompts/runner.md", "LICENSE", "NOTICE", "licenses/codex/LICENSE",
    "licenses/codex/NOTICE", "licenses/ca/MPL-2.0.txt", "licenses/ca/NOTICE",
}
PERSONAL = re.compile(rb'''(?:^|[\s\x00"'=])(?:/home/[^/\s]+/|/Users/[^/\s]+/|[A-Za-z]:[\\/]Users[\\/])''', re.I)
PRIVATE = re.compile(PERSONAL.pattern + rb"|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----", re.I)


class BundleError(ValueError):
    """Only fixed diagnostics and validated public member names reach the CLI."""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def private_path_counts(data):
    """Describe rejected paths using fixed categories, never matched bytes."""
    counts = {key: 0 for key in ("registry", "toolchain", "checkout", "other",
                                "current_home", "current_cargo", "current_rustup", "external")}
    origins = (("current_cargo", Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))),
               ("current_rustup", Path(os.environ.get("RUSTUP_HOME", Path.home() / ".rustup"))),
               ("current_home", Path.home()))
    for match in PERSONAL.finditer(data):
        start = match.start() + len(match[0]) - len(match[0].lstrip(b" \t\r\n\v\f\x00\"'="))
        path = re.split(rb'''[\x00\s"'<>]''', data[start:start + 4096], maxsplit=1)[0]
        normalized = path.replace(b"\\", b"/")
        if b"/registry/src/" in normalized or b"/registry/cache/" in normalized:
            category = "registry"
        elif b"/toolchains/" in normalized or b"/.rustup/" in normalized:
            category = "toolchain"
        elif any(normalized.startswith(str(ROOT).encode().replace(b"\\", b"/") + boundary) for boundary in (b"/", b"=")):
            category = "checkout"
        else:
            category = "other"
        counts[category] += 1
        origin = next((label for label, prefix in origins
                       if any(normalized.startswith(str(prefix).encode().replace(b"\\", b"/") + boundary)
                              for boundary in (b"/", b"="))), "external")
        counts[origin] += 1
    return counts


def version(value):
    if not re.fullmatch(VERSION, value):
        raise BundleError("invalid version")
    return value


def safe_name(name):
    path = PurePosixPath(name)
    return (name == str(path) and not path.is_absolute() and ".." not in path.parts
            and re.fullmatch(r"[A-Za-z0-9_./-]+", name)
            and (name in REQUIRED or name == "MANIFEST" or name.startswith("licenses/"))
            and not re.search(r"token|secret|credential|auth\.json|hassmic", name, re.I))


def config_projection(data):
    text = data.decode()
    parsed = tomllib.loads(text)
    for key, leaf in (("binary", "codex-app-server"), ("ca_file", "cacert.pem")):
        old = parsed["codex"][key]
        new = "/data/local/echo-bridge/bin/" + leaf
        # Match only this key inside its authored table; never change other defaults.
        table = re.search(r"(?ms)^\[codex\]\n.*?(?=^\[|\Z)", text)
        if table is None:
            raise BundleError("missing codex config")
        changed, count = re.subn(r"(?m)^" + key + r'\s*=\s*"' + re.escape(old) + r'"',
                                 key + " = " + json.dumps(new), table[0])
        if count != 1:
            raise BundleError("ambiguous config projection")
        text = text[:table.start()] + changed + text[table.end():]
    return text.encode()


def assemble(out, release, source, codex_commit, codex, bridge, apk, licenses):
    """Only explicit runtime inputs and license texts can enter the archive."""
    version(release)
    payload = {
        "bin/echo-bridge": bridge.read_bytes(),
        "bin/codex-app-server": (codex / "codex-app-server").read_bytes(),
        "bin/cacert.pem": (codex / "cacert.pem").read_bytes(),
        "apk/echo-screen.apk": apk.read_bytes(),
        "scripts/echo-install.sh": (ROOT / "scripts/echo-install.sh").read_bytes(),
        "examples/bridge.echo.toml": config_projection((ROOT / "examples/bridge.echo.toml").read_bytes()),
        "examples/prompts/voice.md": (ROOT / "examples/prompts/voice.md").read_bytes(),
        "examples/prompts/runner.md": (ROOT / "examples/prompts/runner.md").read_bytes(),
        "LICENSE": (ROOT / "LICENSE").read_bytes(), "NOTICE": (ROOT / "NOTICE").read_bytes(),
        "licenses/codex/LICENSE": (codex / "LICENSE").read_bytes(),
        "licenses/codex/NOTICE": (codex / "NOTICE").read_bytes(),
        **licenses,
    }
    manifest = {
        "format": 1, "version": release, "source_commit": source,
        "components": {
            "echo_bridge": {"version": tomllib.loads((ROOT / "crates/echo-bridge/Cargo.toml").read_text())["package"]["version"], "source_commit": source},
            "codex": {"source_commit": codex_commit},
            "echo_screen": {"version": ET.parse(ROOT / "apps/echo-screen/AndroidManifest.xml").getroot().attrib["{http://schemas.android.com/apk/res/android}versionName"]},
            "ca_bundle": {"sha256": digest(payload["bin/cacert.pem"])},
        },
        "files": {name: digest(data) for name, data in sorted(payload.items())},
    }
    payload["MANIFEST"] = json.dumps(manifest, sort_keys=True, indent=2).encode() + b"\n"
    archive = out / f"echo-{release}-armv7.tar.gz"
    with tarfile.open(archive, "w:gz", format=tarfile.USTAR_FORMAT) as target:
        for name, data in sorted(payload.items()):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o755 if name in ("bin/echo-bridge", "bin/codex-app-server", "scripts/echo-install.sh") else 0o644
            target.addfile(info, io.BytesIO(data))
    wrapper = (ROOT / "scripts/install.sh").read_bytes().replace(b"@ECHO_VERSION@", release.encode())
    (out / "install.sh").write_bytes(wrapper)
    (out / "SHA256SUMS").write_text("".join(f"{digest(path.read_bytes())}  {path.name}\n" for path in (archive, out / "install.sh")))
    verify(archive, out / "SHA256SUMS")


def verify(archive, checksums):
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        names = [member.name for member in members]
        if len(set(names)) != len(names) or any(not safe_name(m.name) or not m.isfile() or m.mode not in (0o644, 0o755) for m in members):
            raise BundleError("unsafe archive member")
        payload = {m.name: source.extractfile(m).read() for m in members}
    encoded = payload.pop("MANIFEST")
    if PRIVATE.search(encoded):
        raise BundleError("private manifest data")
    manifest = json.loads(encoded)
    if set(manifest) != {"format", "version", "source_commit", "components", "files"} or manifest["format"] != 1:
        raise BundleError("invalid manifest format")
    release = version(manifest["version"])
    if archive.name != f"echo-{release}-armv7.tar.gz" or not re.fullmatch(r"[0-9a-f]{40}", manifest["source_commit"]):
        raise BundleError("invalid bundle identity")
    if not REQUIRED <= payload.keys() or manifest["files"] != {name: digest(data) for name, data in payload.items()}:
        raise BundleError("manifest membership or digest mismatch")
    components = manifest["components"]
    if set(components) != {"echo_bridge", "codex", "echo_screen", "ca_bundle"}:
        raise BundleError("invalid component provenance")
    if components["echo_bridge"]["source_commit"] != manifest["source_commit"] or not re.fullmatch(r"[0-9a-f]{40}", components["codex"]["source_commit"]):
        raise BundleError("invalid source provenance")
    if set(components["echo_bridge"]) != {"version", "source_commit"} or set(components["codex"]) != {"source_commit"} or set(components["echo_screen"]) != {"version"}:
        raise BundleError("invalid component fields")
    if not all(isinstance(components[key]["version"], str) and components[key]["version"] for key in ("echo_bridge", "echo_screen")):
        raise BundleError("missing component version")
    if components["ca_bundle"] != {"sha256": digest(payload["bin/cacert.pem"])}:
        raise BundleError("invalid CA provenance")
    for name, data in payload.items():
        binary = name in ("bin/echo-bridge", "bin/codex-app-server", "apk/echo-screen.apk")
        # Crypto executables contain PEM parser delimiters, not embedded key material.
        if (PERSONAL if binary else PRIVATE).search(data):
            if binary:
                print("release-kit: rejected path categories " + json.dumps(private_path_counts(data), sort_keys=True), file=sys.stderr)
            raise BundleError("private path or key in " + name)
        if not binary:
            if re.search(rb'(?:Bearer\s+[A-Za-z0-9_-]{16,}|sk-[A-Za-z0-9_-]{20,})', data):
                raise BundleError("credential-shaped payload")
    expected = {archive.name: digest(archive.read_bytes()), "install.sh": digest((checksums.parent / "install.sh").read_bytes())}
    actual = {}
    for line in checksums.read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in actual:
            raise BundleError("invalid checksums")
        actual[match[2]] = match[1]
    if actual != expected:
        raise BundleError("asset checksum mismatch")
    if PRIVATE.search((checksums.parent / "install.sh").read_bytes()):
        raise BundleError("private path in installer")
    return manifest


def license_inputs(codex_work, env):
    """Collect attribution from resolved packages, never copy their build trees."""
    result = {}
    expressions = set()
    for label, root in (("echo", ROOT), ("codex", codex_work / "source/codex-rs")):
        command = ["cargo", "+1.100.0", "metadata", "--locked", "--format-version", "1", "--filter-platform", "armv7-linux-androideabi"]
        metadata = json.loads(subprocess.check_output(command, cwd=root, env=env))
        for package in metadata["packages"]:
            identity = package["name"] + "-" + package["version"]
            prefix = f'licenses/{label}/' + digest(identity.encode())[:16]
            declaration = package.get("license") or "See accompanying license file"
            expressions.update(re.findall(r"[A-Za-z0-9.+-]+", declaration) if package.get("license") else [])
            result[prefix + "/NOTICE"] = (package["name"] + " " + package["version"] + "\n" + declaration + "\n" + (package.get("repository") or "") + "\n").encode()
            directory = Path(package["manifest_path"]).parent
            candidates = list(directory.iterdir())
            if package.get("license_file"):
                candidates.append(directory / package["license_file"])
            for path in candidates:
                if path.is_file() and re.fullmatch(r"(?:LICEN[CS]E|COPYING|NOTICE)(?:[._-][A-Za-z0-9_.-]+)?", path.name, re.I):
                    result[prefix + "/" + path.name] = path.read_bytes()
    # Standard license texts cover declarations even where registry packages omit a file.
    for identifier in sorted(expressions - {"AND", "OR", "WITH"}):
        url = f"https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/text/{identifier}.txt"
        try:
            with urllib.request.urlopen(url, timeout=60) as response:
                result[f"licenses/spdx/{identifier}.txt"] = response.read()
        except OSError:
            raise BundleError("license text download failed for " + identifier) from None
    with urllib.request.urlopen("https://www.mozilla.org/media/MPL/2.0/index.txt", timeout=60) as response:
        result["licenses/ca/MPL-2.0.txt"] = response.read()
    result["licenses/ca/NOTICE"] = b"Mozilla CA certificate store, distributed by curl.se. Mozilla Public License 2.0.\nSee cacert.pem for its generated date and upstream attribution.\n"
    return result


def build_environment(work):
    # Native dependencies embed source paths and literal compiler flags.
    # Keep their sources outside user directories; OpenSSL Configure rejects
    # bare response-file arguments before the compiler can expand them.
    cargo_home = work / "cargo-home"
    cargo_home.mkdir(mode=0o700)
    env = dict(os.environ, CARGO_HOME=str(cargo_home),
               CODEX_ARMV7_WORK_DIR=str(work / "codex-work"),
               CODEX_ARMV7_OUTPUT_DIR=str(work / "codex-out"))
    flags = [flag for flag in env.get("CARGO_ENCODED_RUSTFLAGS", "").split("\x1f") if flag]
    flags += ["--remap-path-prefix=" + str(Path.home()) + "=/build/user",
              "--remap-path-prefix=" + str(ROOT) + "=/build/echo"]
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    return env


def build(args):
    if sys.platform != "linux":
        raise BundleError("Linux is required")
    out = args.out.resolve()
    if out.exists():
        raise BundleError("output directory must not exist; preserve or remove prior assets explicitly")
    out.parent.mkdir(parents=True, exist_ok=True)
    # One owned stage makes publication atomic; build failures expose no new dist assets.
    # Native source and build paths must stay outside a user's TMPDIR/home.
    with tempfile.TemporaryDirectory(prefix="echo-release-", dir="/tmp") as work, tempfile.TemporaryDirectory(prefix=".echo-dist-", dir=out.parent) as stage:
        work = Path(work)
        env = build_environment(work)
        # build-echo deliberately builds offline. Provision this fresh cache,
        # rather than relying on CI setup's separate default Cargo home.
        subprocess.run(["cargo", "+1.100.0", "fetch", "--locked"], cwd=ROOT, env=env, check=True)
        subprocess.run(["make", "build-echo", "ECHO_PACKAGES=echo-bridge"], cwd=ROOT, env=env, check=True)
        result = subprocess.run(["make", "--no-print-directory", "-s", "build-codex-armv7"], cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE, text=True)
        identity = re.findall(r"(?m)^source_commit=([0-9a-f]{40})$", result.stdout)
        if len(identity) != 1:
            raise BundleError("missing Codex build identity")
        subprocess.run(["make", "build-echo-screen"], cwd=ROOT, env=env, check=True)
        subprocess.run([sys.executable, "tests/verify_echo_screen_apk.py"], cwd=ROOT, env=env, check=True)
        source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        target = Path(env.get("CARGO_TARGET_DIR", ROOT / "target"))
        print("release-kit: collecting license texts", flush=True)
        licenses = license_inputs(work / "codex-work", env)
        print("release-kit: assembling and verifying bundle", flush=True)
        assemble(Path(stage), args.version, source, identity[0], work / "codex-out",
                 target / "armv7-linux-androideabi/release/echo-bridge", ROOT / "target/echo-screen/echo-screen.apk",
                 licenses)
        os.rename(stage, out)
        # TemporaryDirectory tolerates the moved stage on exit.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    builder = commands.add_parser("build")
    builder.add_argument("--version", required=True)
    builder.add_argument("--out", type=Path, default=ROOT / "dist")
    verifier = commands.add_parser("verify")
    verifier.add_argument("archive", type=Path)
    verifier.add_argument("--checksums", required=True, type=Path)
    args = parser.parse_args()
    if args.command == "build":
        try:
            version(args.version)
        except ValueError as error:
            parser.error(str(error))
    try:
        if args.command == "build":
            build(args)
        else:
            verify(args.archive, args.checksums)
    except BundleError as error:
        print("release-kit: " + str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, tarfile.TarError, subprocess.SubprocessError):
        print("release-kit: build, input, integrity or privacy check failed", file=sys.stderr)
        return 1
    print("release-kit: bundle=PASS manifest=PASS privacy=PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
