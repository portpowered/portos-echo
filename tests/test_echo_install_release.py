"""Exercise release retention and rollback through the authored device shell."""
import hashlib
import json
import os
import subprocess
from test_echo_install import Sandbox, require, set_owner
from test_echo_install_lifecycle import device_archive

def payload_bytes(root):
    return {str(path.relative_to(root)): path.read_bytes()
            for name in ("bin", "apk", "scripts", "MANIFEST")
            for path in ([root / name] if (root / name).is_file()
                         else (root / name).rglob("*")) if path.is_file()}

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
        require(payload_bytes(previous) == original, "previous payload differs")
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
        require(payload_bytes(previous) == original, "resumed install lost the rollback payload")
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
