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
import zipfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*)?"
COMPANION_VERSION = "0.2.2"
COMPANION_ASSET = f"companion-{COMPANION_VERSION}.screenpack.tar.gz"
REQUIRED = {
    "bin/echo-bridge", "bin/codex-app-server", "bin/cacert.pem", "apk/echo-screen.apk",
    "scripts/echo-install.sh", "examples/bridge.echo.toml", "examples/prompts/voice.md",
    "examples/prompts/runner.md", "LICENSE", "NOTICE", "licenses/codex/LICENSE",
    "licenses/codex/NOTICE", "licenses/ca/MPL-2.0.txt", "licenses/ca/NOTICE",
}
SETUP_SCRIPTS = {"scripts/echo-install-release.sh", "scripts/echo-install-setup.sh"}
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
            and (name in REQUIRED or name in SETUP_SCRIPTS or name == "MANIFEST" or name.startswith("licenses/"))
            and not re.search(r"token|secret|credential|auth\.json|hassmic", name, re.I))


def screen_pack_pin(asset, release, sha):
    return {"url": f"https://github.com/portpowered/portos-echo/releases/download/v{release}/{asset}", "sha256": sha}


def config_projection(data, release, pack_sha):
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
    table = re.search(r"(?ms)^\[screen\]\n.*?(?=^\[|\Z)", text)
    if table is None:
        raise BundleError("missing screen config")
    pin = screen_pack_pin(f"default-{release}.screenpack.tar.gz", release, pack_sha)
    value = 'packs = [{ url = ' + json.dumps(pin["url"]) + ', sha256 = ' + json.dumps(pin["sha256"]) + ' }]'
    changed, count = re.subn(r"(?m)^packs\s*=.*$", value, table[0])
    if count != 1:
        raise BundleError("ambiguous screen config projection")
    return (text[:table.start()] + changed + text[table.end():]).encode()


def screen_pack_payload(archive, name, release):
    """Read the separate generated asset without extracting it into the bundle."""
    if (archive.name != f"{name}-{release}.screenpack.tar.gz" or archive.is_symlink()
            or not archive.is_file() or archive.stat().st_size > 65 * 1024**2):
        raise BundleError(f"invalid {name} archive")
    clock_css = "companion-clock.css" if (name, release) == ("companion", COMPANION_VERSION) else None
    stylesheet = "pack.css" if name == "default" else clock_css
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        if (len(members) > 64 or sum(m.size for m in members) > 64 * 1024**2
                or any(not m.isfile() or m.size > 8 * 1024**2 or (m.name != stylesheet
                       and not re.fullmatch(r"[a-z][a-z0-9-]*\.(?:json|png|webm|mp4)", m.name)) for m in members)):
            raise BundleError(f"invalid {name} members")
        files = {m.name: source.extractfile(m).read() for m in members}
    if len(files) != len(members):
        raise BundleError(f"duplicate {name} member")
    manifest = json.loads(files["pack.json"])
    character = manifest.get("character") if isinstance(manifest, dict) else None
    if (not isinstance(character, dict) or character.get("rest") != "rest.png" or "rest.png" not in files
            or manifest.get("format") != 1 or manifest.get("name") != name
            or manifest.get("version") != release):
        raise BundleError(f"invalid {name} identity")
    layout = manifest.get("layout")
    if clock_css and (clock_css not in files or not isinstance(layout, dict) or layout.get("css") != clock_css):
        raise BundleError("invalid companion stylesheet")
    return files


def exclude_screen_pack(name, files, binaries):
    for data in files.values():
        if any(data in binary for binary in binaries):
            raise BundleError(f"{name} data embedded in executable or JavaScript")


def exclude_companion_from_apk(files, data):
    if any(content in data for content in files.values()):
        raise BundleError("companion data embedded in APK")
    apk = io.BytesIO(data)
    if zipfile.is_zipfile(apk):
        with zipfile.ZipFile(apk) as source:
            for member in source.infolist():
                entry = source.read(member)
                if any(content in entry for content in files.values()):
                    raise BundleError("companion data embedded in APK")


def installer_names(release):
    return (f"echo-setup-{release}-linux-x86_64", f"echo-setup-{release}-windows-x86_64.exe")


def checksum_assets(archive, release, pack_asset, has_setup=True):
    setup = (*installer_names(release), "install.sh", "install.ps1") if has_setup else ("install.sh",)
    return (archive.name, *setup, pack_asset)


def assemble(out, release, source, codex_commit, codex, bridge, apk, licenses, installers):
    """Only explicit runtime inputs and license texts can enter the archive."""
    version(release)
    default_pack = out / f"default-{release}.screenpack.tar.gz"
    companion = out / COMPANION_ASSET
    if companion.is_symlink() or not companion.is_file() or companion.stat().st_size > 65 * 1024**2:
        raise BundleError("invalid companion asset membership")
    payload = {
        "bin/echo-bridge": bridge.read_bytes(),
        "bin/codex-app-server": (codex / "codex-app-server").read_bytes(),
        "bin/cacert.pem": (codex / "cacert.pem").read_bytes(),
        "apk/echo-screen.apk": apk.read_bytes(),
        "scripts/echo-install-setup.sh": (ROOT / "scripts/echo-install-setup.sh").read_bytes(),
        "scripts/echo-install-release.sh": (ROOT / "scripts/echo-install-release.sh").read_bytes(),
        "scripts/echo-install.sh": (ROOT / "scripts/echo-install.sh").read_bytes(),
        "examples/bridge.echo.toml": config_projection((ROOT / "examples/bridge.echo.toml").read_bytes(), release, digest(default_pack.read_bytes())),
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
    for name, binary in zip(installer_names(release), installers, strict=True):
        shutil.copyfile(binary, out / name)
        (out / name).chmod(0o755)
    for name in ("install.sh", "install.ps1"):
        wrapper = (ROOT / "scripts" / name).read_bytes().replace(b"@ECHO_VERSION@", release.encode())
        (out / name).write_bytes(wrapper)
    (out / "SHA256SUMS").write_text("".join(
        f"{digest((out / name).read_bytes())}  {name}\n"
        for name in (*checksum_assets(archive, release, default_pack.name), COMPANION_ASSET)))
    verify(archive, out / "SHA256SUMS", require_companion=True)


def verify(archive, checksums, *, require_companion=False):
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
    if SETUP_SCRIPTS & payload.keys() and not SETUP_SCRIPTS <= payload.keys():
        raise BundleError("incomplete setup runtime")
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
        if not binary and re.search(rb'(?:Bearer\s+[A-Za-z0-9_-]{16,}|sk-[A-Za-z0-9_-]{20,})', data):
            raise BundleError("credential-shaped payload")
    has_setup = SETUP_SCRIPTS <= payload.keys()
    pack_name, pack_path, pack_sha, companion = verify_asset_digests(archive, checksums, release, has_setup, require_companion)
    config = tomllib.loads(payload["examples/bridge.echo.toml"].decode())
    if config["screen"]["packs"] != [screen_pack_pin(pack_path.name, release, pack_sha)]:
        raise BundleError(f"{pack_name} config pin mismatch")
    exclude_screen_pack(pack_name, screen_pack_payload(pack_path, pack_name, release), (payload["bin/echo-bridge"],))
    if companion is not None:
        art = screen_pack_payload(companion, "companion", COMPANION_VERSION)
        exclude_screen_pack("companion", art, (payload["bin/echo-bridge"],))
        exclude_companion_from_apk(art, payload["apk/echo-screen.apk"])
    for name in (("install.sh", "install.ps1") if has_setup else ("install.sh",)):
        if PRIVATE.search((checksums.parent / name).read_bytes()):
            raise BundleError("private path in installer")
    for name in installer_names(release) if has_setup else ():
        data = (checksums.parent / name).read_bytes()
        if PERSONAL.search(data):
            print("release-kit: rejected path categories " + json.dumps(private_path_counts(data), sort_keys=True), file=sys.stderr)
            raise BundleError("private path in " + name)
    print("release-kit: verified binary paths checkout=0 current_home=0", flush=True)
    return manifest


def verify_asset_digests(archive, checksums, release, has_setup, require_companion):
    actual = {}
    for line in checksums.read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in actual:
            raise BundleError("invalid checksums")
        actual[match[2]] = match[1]
    # Historical distributions keep their original asset name and TOML pin.
    pack_name = "template" if f"template-{release}.screenpack.tar.gz" in actual else "default"
    pack_asset = f"{pack_name}-{release}.screenpack.tar.gz"
    names = checksum_assets(archive, release, pack_asset, has_setup)
    if not has_setup and any(os.path.lexists(checksums.parent / name) for name in (*installer_names(release), "install.ps1")):
        raise BundleError("invalid setup asset membership")
    companion = checksums.parent / COMPANION_ASSET
    if require_companion or COMPANION_ASSET in actual or os.path.lexists(companion):
        if (not has_setup or companion.is_symlink() or not companion.is_file() or COMPANION_ASSET not in actual
                or companion.stat().st_size > 65 * 1024**2):
            raise BundleError("invalid companion asset membership")
        names += (COMPANION_ASSET,)
    paths = {name: archive if name == archive.name else checksums.parent / name for name in names}
    if actual.keys() != paths.keys() or any(path.is_symlink() or not path.is_file() for path in paths.values()):
        raise BundleError("asset checksum mismatch")
    if paths[pack_asset].stat().st_size > 65 * 1024**2:
        raise BundleError(f"invalid {pack_name} archive")
    if actual != {name: digest(path.read_bytes()) for name, path in paths.items()}:
        raise BundleError("asset checksum mismatch")
    return pack_name, paths[pack_asset], actual[pack_asset], paths.get(COMPANION_ASSET)


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


def screen_bundle(env):
    directory = ROOT / "web/screen"
    # Provision locked packages explicitly; the project's normal npm mode is offline.
    subprocess.run(["npm", "ci", "--ignore-scripts", "--offline=false"], cwd=directory, env=env, check=True)
    subprocess.run(["npm", "run", "build"], cwd=directory, env=env, check=True)
    env["ECHO_SCREEN_BUNDLE"] = str(directory / "dist/screen.js")


def screen_proof():
    """Compile once, then exercise prebuilt HTTP witnesses outside the checkout."""
    env = dict(os.environ)
    # Keep the ordinary developer path covered before compiling the embedded form.
    env.pop("ECHO_SCREEN_BUNDLE", None)
    subprocess.run(["cargo", "test", "--offline", "--locked", "-p", "echo-screen-server"],
                   cwd=ROOT, env=env, check=True)
    screen_bundle(env)
    result = subprocess.run(["cargo", "test", "--offline", "--locked", "-p", "echo-screen-server",
                             "--no-run", "--message-format=json"],
                            cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE, text=True)
    artifacts = [json.loads(line) for line in result.stdout.splitlines()]
    binaries = [(item["target"]["name"], Path(item["executable"]).resolve()) for item in artifacts
                if item.get("reason") == "compiler-artifact" and item.get("executable")
                and item["profile"]["test"]]
    if not any(name == "screen_t3" for name, _ in binaries):
        raise BundleError("missing prebuilt screen witnesses")
    with tempfile.TemporaryDirectory(prefix="echo-screen-proof-", dir="/tmp") as temporary:
        env["ECHO_SCREEN_TEST_CWD"] = temporary
        for name, binary in binaries:
            observed = subprocess.run([str(binary)], cwd=temporary, env=env, check=True,
                                      timeout=60, stdout=subprocess.PIPE, text=True)
            print(observed.stdout, end="", flush=True)
            if name == "screen_t3" and "test t3_embedded_javascript_is_served_outside_the_checkout ... ok" not in observed.stdout:
                raise BundleError("embedded screen HTTP witness did not execute")
    print("release-kit: developer_screen=PASS embedded_screen_http_outside_checkout=PASS", flush=True)


def build_installers(work, release, env):
    target = work / "setup-target"
    setup_env = dict(env, ECHO_SETUP_RELEASE_VERSION=release, CARGO_TARGET_DIR=str(target),
                     CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="x86_64-w64-mingw32-gcc")
    artifacts = []
    for triple, name in (("x86_64-unknown-linux-gnu", "echo-setup"),
                         ("x86_64-pc-windows-gnu", "echo-setup.exe")):
        subprocess.run(["cargo", "+1.100.0", "build", "--locked", "--release", "-p", "echo-setup",
                        "--bin", "echo-setup", "--target", triple], cwd=ROOT, env=setup_env, check=True)
        artifacts.append(target / triple / "release" / name)
    return artifacts


def stage_screen_pack(stage, work, name, release, env=None):
    generated = work / name
    generator = "companion" if name == "companion" else "template"
    args = ["sh", f"scripts/make-{generator}-screenpack.sh", "--version", release, "--out", str(generated)]
    if generator == "template":
        args += ["--name", name]
    subprocess.run(args, cwd=ROOT, env=env, check=True)
    archive = stage / f"{name}-{release}.screenpack.tar.gz"
    shutil.copyfile(generated / archive.name, archive)
    return archive


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
        stage = Path(stage)
        env = build_environment(work)
        installers = build_installers(work, args.version, env)
        screen_bundle(env)
        javascript = (ROOT / "web/screen/dist/screen.js").read_bytes()
        for name, pack_version in (("default", args.version), ("companion", COMPANION_VERSION)):
            archive = stage_screen_pack(stage, work, name, pack_version, env)
            exclude_screen_pack(name, screen_pack_payload(archive, name, pack_version), (javascript,))
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
        assemble(stage, args.version, source, identity[0], work / "codex-out",
                 target / "armv7-linux-androideabi/release/echo-bridge", ROOT / "target/echo-screen/echo-screen.apk",
                 licenses, installers)
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
    verifier.add_argument("--require-companion", action="store_true")
    commands.add_parser("screen-proof")
    args = parser.parse_args()
    if args.command == "build":
        try:
            version(args.version)
        except ValueError as error:
            parser.error(str(error))
    try:
        if args.command == "build":
            build(args)
        elif args.command == "verify":
            verify(args.archive, args.checksums, require_companion=args.require_companion)
        else:
            screen_proof()
    except BundleError as error:
        print("release-kit: " + str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, tarfile.TarError, zipfile.BadZipFile, subprocess.SubprocessError):
        print("release-kit: build, input, integrity or privacy check failed", file=sys.stderr)
        return 1
    if args.command != "screen-proof":
        print("release-kit: bundle=PASS manifest=PASS privacy=PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
