"""Prepare an official Codex release checkout for the Android ARMv7 build.

These steps replace the release-specific patch files 0001, 0002 and 0004. Each
finds its place by content, so it applies to whichever release the build
selected, and each stops the build when its anchor is gone. ADR 0028 rules:
external locked dependencies are preserved, mode constants are only widened
losslessly, and no filesystem behaviour changes.

    prepare_source.py <codex checkout>
    prepare_source.py --mxc <work directory>
"""
import json
from pathlib import Path
import re
import shutil
import sys
import tomllib

PLACEHOLDER = "0.0.0"
VENDORED = 'openssl-sys = { workspace = true, features = ["vendored"] }'
ANDROID_TABLE = "[target.armv7-linux-androideabi.dependencies]"
MODE_FILE = "codex-rs/exec-server/src/no_follow/unix.rs"
BARE_MODE = re.compile(r"(?<!u32::from\()libc::(S_IF[A-Z]+)")
MXC_SOURCE = "git+https://github.com/microsoft/mxc?rev="
MXC_HOSTS = 'any(target_os = "windows", target_os = "linux", target_os = "macos")'
MXC_CALL = re.compile(r"^([ \t]*)crate::mxc_engine::run::\w+\(.*\);$")
FUNCTION = re.compile(r"^(?:pub(?:\([a-z]+\))? )?(?:async )?fn ")


class PrepareError(Exception):
    pass


def packages(lock_text):
    return tomllib.loads(lock_text)["package"]


def external(lock_text):
    return [package for package in packages(lock_text) if "source" in package]


def align_lock(root):
    """Give every local workspace entry in Cargo.lock the workspace version.

    A release tag sets the workspace version in Cargo.toml and leaves the lock's
    local entries at the placeholder, so `cargo --locked` refuses the tree."""
    manifest = tomllib.loads((root / "codex-rs/Cargo.toml").read_text())
    expected = manifest["workspace"]["package"]["version"]
    path = root / "codex-rs/Cargo.lock"
    original = path.read_text()
    local = {package["name"] for package in packages(original) if "source" not in package}
    blocks = original.split("\n[[package]]\n")
    for index, block in enumerate(blocks[1:], start=1):
        if re.search(r'^source = "', block, re.M):
            continue
        blocks[index] = align_block(block, expected)
    aligned = "\n[[package]]\n".join(blocks)
    # A dependency is listed as "name version" only when two versions of it are locked.
    aligned = re.sub(r'^ "([^" ]+) ' + re.escape(PLACEHOLDER) + r'"',
                     lambda match: f' "{match[1]} {expected}"' if match[1] in local else match[0],
                     aligned, flags=re.M)
    if external(aligned) != external(original):
        raise PrepareError("lock alignment changed an external dependency")
    for package in packages(aligned):
        if "source" not in package and package["version"] != expected:
            raise PrepareError(f"local package {package['name']} is locked at {package['version']}; expected {expected}")
    return (path, aligned), expected, len(local)


def align_block(block, expected):
    match = re.search(r'^version = "([^"]+)"$', block, re.M)
    name = re.search(r'^name = "([^"]+)"$', block, re.M)
    if not match or not name:
        raise PrepareError("unreadable local lock entry")
    if match[1] not in (PLACEHOLDER, expected):
        raise PrepareError(f"local package {name[1]} is locked at {match[1]}; expected {expected}")
    return block[:match.start(1)] + expected + block[match.end(1):]


def vendor_openssl(root):
    """Build OpenSSL from source for Android ARMv7 only, as upstream does for musl.

    The build has no static OpenSSL prefix for this target. TLS verification and
    the CA bundle are unchanged, and no other target's dependencies change."""
    path = root / "codex-rs/core/Cargo.toml"
    text = path.read_text()
    if VENDORED not in text.splitlines():
        raise PrepareError("OpenSSL anchor missing: core/Cargo.toml no longer vendors openssl-sys for any target")
    if ANDROID_TABLE in text:
        raise PrepareError("OpenSSL step is stale: core/Cargo.toml already has an Android ARMv7 dependency table")
    addition = ("\n# Build OpenSSL from source for the Android ARMv7 deployment.\n"
                + ANDROID_TABLE + "\n" + VENDORED + "\n")
    prepared = text.rstrip("\n") + "\n" + addition
    before, after = tomllib.loads(text), tomllib.loads(prepared)
    added = after["target"].pop("armv7-linux-androideabi")
    if after != before or added != {"dependencies": {"openssl-sys": {"workspace": True, "features": ["vendored"]}}}:
        raise PrepareError("OpenSSL step changed more than the Android ARMv7 dependency table")
    return path, prepared


def widen_modes(root):
    """Widen libc mode constants to u32 on the statx and fstatat paths.

    Android ARMv7 libc mode constants are u16 while the mode fields compared
    with them are u32. `u32::from` is lossless on every target; the masks, the
    syscalls and the symlink rejection are unchanged."""
    path = root / MODE_FILE
    if not path.is_file():
        raise PrepareError("mode-width anchor missing: " + MODE_FILE)
    text = path.read_text()
    anchors = {"statx": "u32::from(metadata.stx_mode) & libc::S_IFMT",
               "fstatat": "metadata.st_mode & libc::S_IFMT"}
    for name, anchor in anchors.items():
        if anchor not in text:
            raise PrepareError(f"mode-width anchor missing: the {name} path no longer reads `{anchor}`")
    allowed = re.compile(r"(?:& |kind == )libc::S_IF[A-Z]+")
    if len(allowed.findall(text)) != len(BARE_MODE.findall(text)):
        raise PrepareError("mode-width step is stale: a libc mode constant is used outside a mask or comparison")
    widened = BARE_MODE.sub(r"u32::from(libc::\1)", text)
    if BARE_MODE.search(widened) or widened.count("u32::from(libc::S_IF") != len(BARE_MODE.findall(text)):
        raise PrepareError("mode-width step did not widen every constant")
    return (path, widened), len(BARE_MODE.findall(text))


def gate_mxc(sdk):
    """Configure out, on hosts without it, the engine's calls into its `run` module.

    The locked SDK compiles `mod run` only for Windows, Linux and macOS, yet two
    dispatch functions name it unconditionally, so no other target compiles.
    Both first return an error on such a host, so the calls were unreachable
    there. On the three supported hosts the compiled code is unchanged."""
    engine = sdk / "src/core/mxc_engine"
    module, dispatch = engine / "mod.rs", engine / "dispatch.rs"
    if not module.is_file() or not dispatch.is_file():
        raise PrepareError("mxc anchor missing: the SDK has no engine dispatch module")
    text = dispatch.read_text()
    refusal = f"#[cfg(not({MXC_HOSTS}))]"
    if f"#[cfg({MXC_HOSTS})]\nmod run;" not in module.read_text() or refusal not in text:
        raise PrepareError("mxc step is stale: the engine no longer configures `run` out by host")
    prepared, checked, count = [], False, 0
    for line in text.split("\n"):
        checked = "ensure_host_supported()?;" in line or (checked and not FUNCTION.match(line))
        if "crate::mxc_engine::run::" in line and (not prepared or prepared[-1].strip() != f"#[cfg({MXC_HOSTS})]"):
            call = MXC_CALL.match(line)
            if not call or not checked:
                raise PrepareError("mxc step is stale: a use of `run` is not a statement behind the host check")
            prepared.append(f"{call[1]}#[cfg({MXC_HOSTS})]")
            count += 1
        prepared.append(line)
    return (dispatch, "\n".join(prepared)), count


def vendor_mxc(work):
    """Vendor the exact mxc-sdk checkout cargo locked, with `gate_mxc` applied.

    `metadata.json` is `cargo metadata --locked` for the Android target, so the
    checkout is the one the release's lock names. The override is a private
    `paths` entry beside pagable's; Cargo.lock is not touched."""
    packages = json.loads((work / "metadata.json").read_text())["packages"]
    found = [package for package in packages if package["name"] == "mxc-sdk"]
    if not found:
        print("prepare-source: mxc-sdk is not locked by this release")
        return
    if len(found) != 1 or not (found[0].get("source") or "").startswith(MXC_SOURCE):
        raise PrepareError("mxc step is stale: mxc-sdk is not the single locked git checkout")
    sdk = Path(found[0]["manifest_path"]).parent
    root = next((parent for parent in (sdk, *sdk.parents) if (parent / ".cargo-ok").is_file()), None)
    if root is None:
        raise PrepareError("mxc anchor missing: mxc-sdk is not in a cargo git checkout")
    _, count = gate_mxc(sdk)
    if count == 0:
        print("prepare-source: mxc-sdk needs no Android gate")
        return
    vendor = work / "vendor/mxc"
    vendor.parent.mkdir(exist_ok=True)
    shutil.copytree(root, vendor, symlinks=True, ignore=shutil.ignore_patterns(".git"))
    (path, text), count = gate_mxc(vendor / sdk.relative_to(root))
    path.write_text(text)
    config = work / "cargo-config.toml"
    paths = tomllib.loads(config.read_text())["paths"] if config.is_file() else []
    paths.append(str(vendor / sdk.relative_to(root)))
    config.write_text("paths = [" + ", ".join(json.dumps(path) for path in paths) + "]\n")
    print(f"prepare-source: mxc-sdk={found[0]['source'].rsplit('#', 1)[-1]} android_gates={count}")


def main(arguments):
    if len(arguments) == 2 and arguments[0] == "--mxc":
        return vendor_mxc(Path(arguments[1]))
    if len(arguments) != 1:
        raise PrepareError("usage: prepare_source.py <codex checkout> | --mxc <work directory>")
    root = Path(arguments[0])
    lock, version, local = align_lock(root)
    manifest = vendor_openssl(root)
    modes, widened = widen_modes(root)
    # Nothing is written unless every step found its anchor.
    for path, text in (lock, manifest, modes):
        path.write_text(text)
    print(f"prepare-source: version={version} local_lock_entries={local} openssl=vendored-armv7 mode_constants_widened={widened}")


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except PrepareError as error:
        print(f"build-codex-armv7: {error}", file=sys.stderr)
        sys.exit(1)
