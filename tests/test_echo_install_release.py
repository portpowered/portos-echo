"""Exercise release retention and rollback through the authored device shell."""
import hashlib
import json
import os
import subprocess
from test_echo_install import Sandbox, require, set_owner
from test_echo_install_lifecycle import device_archive

def public_bytes(box):
    return {str(path.relative_to(box.device)): path.read_bytes()
            for name in ("bin", "apk", "scripts", "MANIFEST")
            for path in ([box.device / name] if (box.device / name).is_file()
                         else (box.device / name).rglob("*")) if path.is_file()}
def release_cases():
    from test_echo_install_lifecycle import device_archive
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
        original = public_bytes(box)
        box.run(["rollback"], 1)
        require(public_bytes(box) == original, "first rollback changed active release")
        private = {"private/bridge.toml": "edited config", "home/notes.md": "retained notes"}
        for name, value in private.items():
            path = box.device / name
            path.write_text(value)
            set_owner(box.root, path, "9999:9999")
        for name in ("bin/echo-bridge", "bin/codex-app-server", "bin/cacert.pem", "apk/echo-screen.apk", "MANIFEST"):
            (bundle / name).write_text("version B: " + name)
        install("0.2.0")
        current = public_bytes(box)
        install("0.2.0")
        require(public_bytes(box) == current, "same-version retry changed the active payload")
        # Interrupt publication after the binaries are copied, then retry the same release.
        (bundle / "apk").rename(stage / "saved-apk")
        install("0.2.0", 1)
        require((box.device / "releases/pending").read_text().strip() == "retry", "retry was not marked pending")
        box.run(["status"], 1)
        (stage / "saved-apk").rename(bundle / "apk")
        install("0.2.0")
        require(public_bytes(box) == current, "partial retry did not recover the active payload")
        require(not any((box.device / "releases" / name).exists() for name in ("pending", "retry")),
                "retry retained recovery state")
        require(json.loads((box.device / "release.json").read_text()) ==
                {"format": 1, "current": "0.2.0", "previous": "0.1.0"}, "release metadata differs")
        previous = box.device / "releases/previous"
        require({str(path.relative_to(previous)): path.read_bytes()
                 for path in previous.rglob("*") if path.is_file() and path.name not in ("version", "SHA256SUMS")}
                == original, "previous payload differs")
        require(not (previous / "private").exists() and not (previous / "home").exists(),
                "snapshot contains private data")
        saved = (previous / "bin/echo-bridge").read_bytes()
        (previous / "bin/echo-bridge").write_bytes(b"tampered")
        box.run(["rollback"], 1)
        require(public_bytes(box) == current, "tampered rollback changed active files")
        (previous / "bin/echo-bridge").write_bytes(saved)
        box.run(["rollback"])
        require(public_bytes(box) == original, "rollback did not restore every public file")
        require(json.loads((box.device / "release.json").read_text()) ==
                {"format": 1, "current": "0.1.0", "previous": None}, "rollback version differs")
        for name, value in private.items():
            require((box.device / name).read_text() == value, "rollback changed private/home bytes")
        box.run(["rollback"], 1)
        require(not (box.root / "remote/echo-bridge.lock").exists(), "rollback leaked lock")
        box.run(["uninstall"])
        require(not (box.device / "releases").exists(), "uninstall retained release binaries")
    for fault in ("push", "releases/metadata"):
        with Sandbox() as box:
            box.install()
            original = public_bytes(box)
            box.files["bridge"].write_text("version B")
            (box.root / "fault").write_text(fault)
            box.install(code=1)
            require((box.device / "releases/pending").is_file(), "partial install is not marked")
            require(not (box.device / "private/launch.sh").exists(), "partial install left a boot launcher")
            box.run(["status"], 1)
            box.run(["start"], 1)
            box.run(["rollback"])
            require(public_bytes(box) == original, "failed update lost recoverable files")
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
                original = public_bytes(box)
                box.run(["rollback"], 1)
                require(public_bytes(box) == original and outside.read_text() == "outside bytes",
                        "unsafe snapshot changed active or outside bytes")
                require(not (box.root / "remote/echo-bridge.lock").exists(), "unsafe snapshot took lock")
    print("echo-install releases: snapshot=PASS retry=PASS rollback=PASS partial=PASS retention=PASS")
