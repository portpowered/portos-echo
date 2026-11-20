"""Exercise release retention and rollback through the authored device shell."""
import hashlib
import io
import json
import os
import subprocess
import tarfile
from test_echo_install import Sandbox, owner, require, set_owner

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
    for name in ("echo-install-release.sh", "echo-install-setup.sh"):
        helper = box.script.with_name(name)
        helper.write_text(helper.read_text().replace("/data/local", str(box.root / "remote")))
        payload["scripts/" + name] = helper.read_bytes()
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

def payload_bytes(root):
    return {str(path.relative_to(root)): path.read_bytes()
            for name in ("bin", "apk", "scripts", "MANIFEST")
            for path in ([root / name] if (root / name).is_file()
                         else (root / name).rglob("*")) if path.is_file()}

def snapshot_bytes(root):
    return {str(path.relative_to(root)): path.read_bytes()
            for path in root.rglob("*") if path.is_file() and path.name not in ("version", "SHA256SUMS")}

def require_release(box, current, previous):
    observed = json.loads((box.device / "release.json").read_text())
    expected = {"format": 1, "current": current, "previous": previous}
    require(observed == expected, f"release metadata differs: expected {expected}, observed {observed}")

def interrupt_pending_cleanup(box):
    remove = box.root / "remote-tools/rm"
    remove.write_text('''#!/bin/sh
if [ "$#" = 2 ] && [ "$1" = -f ] &&
  [ "$2" = "$FAKE_ROOT/remote/echo-bridge/releases/pending" ]; then
  /usr/bin/rm -f "$0"
  exit 73
fi
exec /usr/bin/rm "$@"
''')
    remove.chmod(0o755)

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
        def install(version, code=0):
            result = subprocess.run(["/bin/sh", str(bundle / "scripts/echo-install.sh"),
                                     "--device", "install", "--bundle", str(bundle),
                                     "--release-version", version], env=env,
                                    capture_output=True, timeout=15)
            require(result.returncode == code, "unexpected install outcome: " + result.stderr.decode())
        install("0.1.0")
        original = payload_bytes(box.device)
        box.run(["rollback"], 1)
        require(payload_bytes(box.device) == original, "first rollback changed active release")
        private = {"private/bridge.toml": "edited config", "home/notes.md": "retained notes"}
        for name, value in private.items():
            path = box.device / name
            path.write_text(value)
            set_owner(box.root, path, "9999:9999")
        for name in ("bin/echo-bridge", "bin/codex-app-server", "bin/cacert.pem", "apk/echo-screen.apk", "MANIFEST"):
            (bundle / name).write_text("version B: " + name)
        install("0.2.0")
        current = payload_bytes(box.device)
        install("0.2.0")
        require(payload_bytes(box.device) == current, "same-version retry changed the active payload")
        pending = box.device / "releases/pending"
        previous = box.device / "releases/previous"
        def interrupt_refresh():
            # Interrupt publication after the binaries are copied.
            (bundle / "apk").rename(stage / "saved-apk")
            install("0.2.0", 1)
            require(pending.read_text().strip() == "retry", "retry was not marked pending")
            box.run(["status"], 1)
            (stage / "saved-apk").rename(bundle / "apk")
        interrupt_refresh()
        install("0.2.0")
        require(payload_bytes(box.device) == current, "partial retry did not recover the active payload")
        retry = box.device / "releases/retry"
        require(not pending.exists() and not retry.exists(), "retry retained recovery state")
        require_release(box, "0.2.0", "0.1.0")
        require(snapshot_bytes(previous) == original, "previous payload differs")
        require(not (previous / "private").exists() and not (previous / "home").exists(),
                "snapshot contains private data")
        interrupt_refresh()
        interrupt_pending_cleanup(box)
        result = box.run(["rollback"], 1)
        require("rolled back" not in result.stdout, "interrupted rollback reported success")
        require_release(box, "0.1.0", None)
        require(payload_bytes(box.device) == original, "interrupted rollback published incorrect files")
        rollback_target = pending.read_text().strip()
        install("0.2.0")
        require_release(box, "0.2.0", "0.1.0")
        require(payload_bytes(box.device) == current, "resumed install published incorrect files")
        require(snapshot_bytes(previous) == original, "resumed install lost the rollback payload")
        require(rollback_target == "previous", "interrupted rollback retained the retry target")
        require(not pending.exists() and not retry.exists(), "resumed install retained recovery state")
        saved = (previous / "bin/echo-bridge").read_bytes()
        (previous / "bin/echo-bridge").write_bytes(b"tampered")
        box.run(["rollback"], 1)
        require(payload_bytes(box.device) == current, "tampered rollback changed active files")
        (previous / "bin/echo-bridge").write_bytes(saved)
        box.run(["rollback"])
        require(payload_bytes(box.device) == original, "rollback did not restore every public file")
        require_release(box, "0.1.0", None)
        for name, value in private.items():
            require((box.device / name).read_text() == value, "rollback changed private/home bytes")
        box.run(["rollback"], 1)
        require(not (box.root / "remote/echo-bridge.lock").exists(), "rollback leaked lock")
        box.run(["uninstall"])
        require(not (box.device / "releases").exists(), "uninstall retained release binaries")
    for fault in ("push", "releases/metadata"):
        with Sandbox() as box:
            box.install()
            original = payload_bytes(box.device)
            box.files["bridge"].write_text("version B")
            (box.root / "fault").write_text(fault)
            box.install(code=1)
            require((box.device / "releases/pending").is_file(), "partial install is not marked")
            require(not (box.device / "private/launch.sh").exists(), "partial install left a boot launcher")
            box.run(["status"], 1)
            box.run(["start"], 1)
            box.run(["rollback"])
            require(payload_bytes(box.device) == original, "failed update lost recoverable files")
            box.files["bridge"].write_text("version C")
            box.install()
            require((box.device / "bin/echo-bridge").read_text() == "version C", "recovery did not install")
            require((box.device / "private/launch.sh").is_file(), "recovery did not restore the launcher")
    for target_name in ("releases/previous/bin/echo-bridge", "MANIFEST"):
        for kind in ("symlink", "hardlink", "fifo", "owner", "mode"):
            with Sandbox() as box:
                box.install()
                box.install()
                target = box.device / target_name
                if not target.exists(): target.write_text("public manifest")
                outside = box.root / "outside"
                outside.write_text("outside bytes")
                if kind in ("symlink", "hardlink", "fifo"):
                    target.unlink()
                    if kind == "symlink": target.symlink_to(outside)
                    elif kind == "hardlink": os.link(outside, target)
                    else: os.mkfifo(target)
                elif kind == "owner": set_owner(box.root, target, "9999:9999")
                else: target.chmod(0o666)
                original = payload_bytes(box.device)
                box.run(["rollback"], 1)
                require(payload_bytes(box.device) == original and outside.read_text() == "outside bytes",
                        "unsafe snapshot changed active or outside bytes")
                require(not (box.root / "remote/echo-bridge.lock").exists(), "unsafe snapshot took lock")
    print("echo-install releases: snapshot=PASS retry=PASS rollback=PASS interrupted_rollback=PASS partial=PASS retention=PASS")
