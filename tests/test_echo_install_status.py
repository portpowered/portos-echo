"""Read-only status, retention and complete CLI journey against recording adb."""
import shutil
import hashlib
import os
import selectors
import json
import subprocess
import time

from test_echo_install import Sandbox, prepare_lifecycle, require, set_owner


def owner_state(box):
    values = {"private/bridge.toml": "owner config", "private/credential": "owner private",
              "home/prompts/voice.md": "owner voice", "home/settings.toml": "owner settings"}
    for name, value in values.items():
        (box.device / name).write_text(value)
        # These are files authored by the existing nobody-owned runtime.
        set_owner(box.root, box.device / name, "9999:9999")
    return values


def retained(box, values):
    for name, value in values.items():
        require((box.device / name).read_text() == value, "owner bytes changed: " + name)


def snapshot(box):
    return {str(path.relative_to(box.device)): (path.stat().st_mode, path.read_bytes())
            for path in box.device.rglob("*") if path.is_file()}


def exited(box):
    for name in ("child-pid", "holder-pid"):
        try:
            fd = os.pidfd_open(int((box.root / name).read_text()))
        except ProcessLookupError:
            continue
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(fd, selectors.EVENT_READ)
                require(bool(selector.select(3)), "journey leaked a fixture process")
        finally:
            os.close(fd)


def status_observation():
    with Sandbox() as box:
        box.install()
        (box.root / "running").touch()
        proc = box.root / "proc/13"
        proc.mkdir(parents=True)
        # Field 22 is starttime: 100 ticks at the controlled 100 Hz clock.
        (proc / "stat").write_text("13 (echo-bridge) S " + "0 " * 18 + "100\n")
        (box.root / "proc/uptime").write_text("43.99 0.00\n")
        getconf = box.root / "remote-tools/getconf"
        getconf.write_text("#!/bin/sh\n[ \"$1\" = CLK_TCK ] || exit 1\nprintf '100\\n'\n")
        getconf.chmod(0o755)
        (box.root / "remote/hassmic/boot.log").write_text(
            "client connected / satellite: running\nother harmless line\n")
        (box.device / "private/bridge.log").write_text("".join(f"line {i}\n" for i in range(6)))
        before = snapshot(box)
        index = len(box.calls())
        result = box.run(["status"])
        require(result.stdout == "version_current=null\nversion_previous=null\nprocess=running\nprocess_user=nobody\nuptime_seconds=42\n"
                "hassmic_link=client connected / satellite: running\nbridge.log (last 5):\n"
                "line 1\nline 2\nline 3\nline 4\nline 5\n", "status observation differs")
        require(before == snapshot(box), "status mutated owner files")
        calls = box.calls()[index:]
        require(all(call[0] in ("devices", "shell") for call in calls), "status pushed files")
        require(not any(word in " ".join(call) for call in calls
                        for word in ("mkdir", "chmod", "touch", "rm -", "kill ")), "status mutated device")
        (proc / "stat").unlink()
        require("uptime_seconds=unavailable\n" in box.run(["status"]).stdout, "missing uptime invented")
    with Sandbox() as box:
        result = box.run(["status"])
        require(result.stdout == "process=stopped\nprocess_user=unavailable\nuptime_seconds=unavailable\nhassmic_link=unavailable\n"
                "bridge.log (last 5):\nunavailable\n", "missing status differs")
        require(not box.device.exists(), "status created device state")
        (box.root / "fault").write_text("ps -A")
        box.run(["status"], 1)
    with Sandbox() as box:
        box.install()
        (box.root / "running").touch()
        before = snapshot(box)
        for user in ("root", "0", "unknown"):
            (box.root / "process-user").write_text(user)
            result = box.run(["status"], 1)
            observed = user if user != "unknown" else "unavailable"
            require("process_user=" + observed + "\n" in result.stdout, "observed identity missing")
            expected = ("echo-bridge is running as root; stop and reinstall before starting"
                        if user != "unknown" else "cannot verify echo-bridge process user")
            require(result.stderr == "echo-install: " + expected + "\n", "identity refusal differs")
            require(snapshot(box) == before, "identity refusal mutated files")
    print("F18 F20: PASS")


def status_privacy():
    # Synthetic strings, assembled so fixtures cannot be mistaken for credentials.
    values = ["token=" + "fixture-one", "Authorization: Bearer " + "fixture-two",
              "API_KEY: " + "fixture-three", "eyJ" + "fixture.payload.signature",
              "x" * 40, '"access_token": "fixture-four"', "password=fixture-five"]
    with Sandbox() as box:
        box.install()
        log = box.device / "private/bridge.log"
        for value in values:
            log.write_text("harmless\n" + value + "\n")
            (box.root / "remote/hassmic/boot.log").write_text("client connected " + value + "\n")
            result = box.run(["status"])
            require(value not in result.stdout + result.stderr, "token-like bytes leaked")
            require(result.stdout.count("[masked]") == 2 and "harmless\n" in result.stdout,
                    "masking or harmless output differs")
        # A failing local sanitizer must not print even partially filtered data.
        awk = box.tools / "awk"
        real = awk.read_text()
        awk.write_text("#!/bin/sh\ncase \"$1\" in *secret=*) printf 'unsafe partial output'; exit 1;; esac\n"
                       + real.split("\n", 1)[1])
        result = box.run(["status"], 1)
        require(not result.stdout and "filter failed" in result.stderr, "filter failure leaked output")
        awk.write_text(real)
        (box.root / "fault").write_text("tail -n 5")
        result = box.run(["status"], 1)
        require(not result.stdout, "adb error leaked status payload")
    print("F19: PASS")


def uninstall_failures():
    for child in ("", "bin", "private", "home", "private/launch.sh"):
        with Sandbox() as box:
            box.install()
            outside = box.root / "outside"
            outside.mkdir()
            sentinel = outside / "sentinel"
            sentinel.write_text("outside owner state")
            path = box.device / child
            if path.is_dir():
                shutil.rmtree(path)
                path.symlink_to(outside, target_is_directory=True)
            else:
                path.unlink()
                path.symlink_to(sentinel)
            box.run(["uninstall", "--purge"], 1)
            box.install(code=1)
            require(sentinel.read_text() == "outside owner state", "symlink escaped owned root")
    for fault in ("rm -rf '/data/local/echo-bridge/bin'", "rm -f '/data/local/echo-bridge/private/launch.sh'",
                  "rm -rf '/data/local/echo-bridge/private'"):
        with Sandbox() as box:
            box.install()
            values = owner_state(box)
            (box.root / "fault").write_text(fault)
            result = box.run(["uninstall", "--purge"], 1)
            require(not result.stdout, "failed removal reported success")
            retained(box, values)
            box.run(["uninstall", "--purge"])
    print("F22: PASS")


def full_journey():
    with Sandbox() as box:
        help_text = box.run(["--help"]).stdout
        require("not yet" not in help_text and "next implementation" not in help_text,
                "help advertises incomplete command")
        for result in ("process=running|stopped", "uptime_seconds=INTEGER|unavailable",
                       "hassmic_link=SANITIZED_LINE|unavailable",
                       "echo-install: uninstalled (state retained)", "echo-install: uninstalled (purged)"):
            require(result in help_text, "help omits declared result: " + result)
        box.prompt("voice.md", "default voice")
        prepare_lifecycle(box)
        values = owner_state(box)
        box.run(["start"])
        require("process=running\n" in box.run(["status"]).stdout, "journey status stopped")
        box.run(["stop"])
        require((box.root / "eof").read_text() == "EOF", "journey child did not see EOF")
        exited(box)
        box.install()
        retained(box, values)
        hassmic = (box.root / "remote/hassmic/runas").read_bytes()
        require(box.run(["uninstall"]).stdout == "echo-install: uninstalled (state retained)\n",
                "uninstall output differs")
        require(not (box.device / "bin").exists() and not (box.device / "private/launch.sh").exists(),
                "ordinary uninstall left executable payloads")
        retained(box, values)
        box.install()
        retained(box, values)
        # Uninstall itself must close stdin and wait for EOF before removing anything.
        (box.root / "eof").unlink()
        box.run(["start"])
        require(box.run(["uninstall", "--purge"]).stdout == "echo-install: uninstalled (purged)\n",
                "purge output differs")
        require((box.root / "eof").read_text() == "EOF", "uninstall skipped clean stop")
        exited(box)
        require(not any((box.device / part).exists() for part in ("bin", "private", "home")),
                "purge retained owned payloads")
        require((box.root / "remote/hassmic/runas").read_bytes() == hassmic, "hassmic changed")
        require(not (box.root / "remote/echo-bridge.lock").exists(), "journey lock leaked")
        require(box.run(["uninstall", "--purge"]).stdout == "echo-install: uninstalled (purged)\n",
                "repeated purge output differs")
    print("F21 F23: PASS")


def status_cases():
    status_observation()
    status_privacy()
    uninstall_failures()
    full_journey()
    doctor_cases()
    release_status_cases()


def release_status_cases():
    """Read the shell's own canonical metadata without mutating retained state."""
    with Sandbox() as box:
        box.install()
        metadata = box.device / "release.json"
        for current, previous in (("0.2.0", "0.1.0"), ("0.2.0-rc.1", None), (None, None)):
            for indent in (None, 2):
                metadata.write_text(json.dumps({"format": 1, "current": current,
                                                "previous": previous}, indent=indent))
                metadata.chmod(0o644)
                set_owner(box.root, metadata, "0:0")
                before = snapshot(box)
                output = box.run(["status"]).stdout
                require(output.startswith(f"version_current={current or 'null'}\n"
                                          f"version_previous={previous or 'null'}\n"),
                        "canonical release versions differ")
                require(snapshot(box) == before, "release observation mutated installed state")
        valid = '{"format":1,"current":"0.2.0","previous":null}'
        malformed = ("", valid + valid, valid.replace('"format":1', '"format":2'),
                     valid.replace('"0.2.0"', '"v0.2.0"'), valid.replace('"0.2.0"', '"0.02.0"'),
                     valid.replace('"0.2.0"', '"0.2.0-01"'), valid.replace('"0.2.0"', '"0.2.0-rc.01"'),
                     valid.replace('"0.2.0"', '"0.2.0\nrc1"'),
                     valid.replace('"0.2.0"', '"SYNTHETIC-TOKEN"'),
                     valid.replace('"previous":null', '"previous":null,"previous":"0.1.0"'),
                     valid.replace('"current"', '"cur\nrent"'), " " * 1025 + valid)
        for contents in malformed:
            metadata.write_text(contents)
            metadata.chmod(0o644)
            set_owner(box.root, metadata, "0:0")
            before = snapshot(box)
            for command in (["status"], ["start"], ["stop"], ["uninstall", "--purge"],
                            box.install_args()):
                result = box.run(command, 1)
                require("SYNTHETIC-TOKEN" not in result.stdout + result.stderr,
                        "malformed metadata leaked")
                require(snapshot(box) == before, "malformed release state permitted mutation")
                require(not (box.root / "remote/echo-bridge.lock").exists(),
                        "malformed release state took mutation lock")
        metadata.write_text(valid)
        for kind in ("owner", "mode", "link", "hardlink", "directory", "fifo"):
            metadata.unlink()
            metadata.write_text(valid)
            metadata.chmod(0o644)
            set_owner(box.root, metadata, "0:0")
            outside = box.root / ("metadata-" + kind)
            outside.write_text(valid)
            if kind == "owner":
                set_owner(box.root, metadata, "9999:9999")
            elif kind == "mode":
                metadata.chmod(0o666)
            else:
                metadata.unlink()
                if kind == "link":
                    metadata.symlink_to(outside)
                elif kind == "hardlink":
                    os.link(outside, metadata)
                elif kind == "directory":
                    metadata.mkdir()
                else:
                    os.mkfifo(metadata)
            for command in (["status"], ["uninstall", "--purge"], box.install_args()):
                box.run(command, 1)
                require(outside.read_text() == valid, "redirected release state changed outside file")
                require((box.device / "bin/echo-bridge").is_file(), "unsafe metadata allowed uninstall")
            if kind == "directory":
                metadata.rmdir()
                metadata.touch()
    print("echo-install release status: versions=PASS malformed=PASS redirected=PASS readonly=PASS")


def doctor_tools(box):
    """Closed external probes: never resolve or connect to a real endpoint."""
    epoch = int(time.time())
    for name, body in {
        "nc": "printf '%s\\n' \"$*\" > \"$FAKE_ROOT/nc-args\"\nexit 0\n",
        "date": f"printf '{epoch}\\n'\n",
        "df": "printf 'Filesystem 1K-blocks Used Available Use%% Mounted\\nfixture 524288 262144 262144 50%% /data\\n'\n",
    }.items():
        tool = box.root / "remote-tools" / name
        tool.write_text("#!/bin/sh\n" + body)
        tool.chmod(0o755)
    return epoch


def doctor_cases():
    with Sandbox() as box:
        box.install()
        epoch = doctor_tools(box)
        (box.root / "running").touch()
        before = snapshot(box)
        expected = (f"hassmic=ok\negress=ok\nclock={epoch}\ndisk_free=262144\n"
                    "process_user=nobody\n")
        require(box.run(["doctor"]).stdout == expected, "healthy doctor probes differ")
        require((box.root / "nc-args").read_text() == "-z -w 5 chatgpt.com 443\n",
                "egress probe is not bounded")
        for user in ("root", "0", "unknown"):
            (box.root / "process-user").write_text(user)
            value = user if user != "unknown" else "unavailable"
            require(box.run(["doctor"]).stdout == expected.replace("process_user=nobody", "process_user=" + value),
                    "unsafe doctor identity differs")
        (box.root / "process-user").unlink()
        for tool, body, field in (
                ("nc", "exit 1\n", "egress=failed"),
                ("date", "printf 'invalid\\n'\n", "clock=unavailable"),
                ("df", "printf 'invalid\\n'\n", "disk_free=unavailable")):
            doctor_tools(box)
            path = box.root / "remote-tools" / tool
            path.write_text("#!/bin/sh\n" + body)
            require(field in box.run(["doctor"]).stdout, "failed doctor probe invented success")
        (box.root / "remote/hassmic/runas").unlink()
        (box.root / "nc-args").unlink()
        require("hassmic=failed\negress=failed\n" in box.run(["doctor"]).stdout,
                "missing hassmic did not fail its checks")
        require(not (box.root / "nc-args").exists(), "egress ran without hassmic")
        require(snapshot(box) == before, "doctor modified installed state")
        require(not (box.root / "remote/echo-bridge.lock").exists(), "doctor took mutation lock")
    print("echo-install doctor: probes=PASS identity=PASS readonly=PASS")


def native_lifecycle_cases(binary):
    """Consume Cargo's prebuilt installer, execute its real streamed device shell."""
    for purge in (False, True):
        with Sandbox() as box:
            box.install()
            values = owner_state(box)
            hassmic = (box.root / "remote/hassmic/runas").read_bytes()
            before = snapshot(box)

            def native(command, *extra, code=0):
                result = subprocess.run(
                    [binary, command, "--json", "--adb", str(box.tools / "adb"), *extra],
                    env=box.env, capture_output=True, text=True, timeout=10)
                for marker in ("synthetic-private-content-do-not-print", str(box.root)):
                    require(marker not in result.stdout + result.stderr, "native output leaked private data")
                require(result.returncode == code, "native lifecycle exit differs: " + command + " " + result.stdout)
                return json.loads(result.stdout)

            result = native("status")
            require(result["process"] == "stopped" and result["process_user"] == "unavailable",
                    "native stopped observation differs")
            require(result["versions"] == {"current": None, "previous": None}, "invented versions")
            require(snapshot(box) == before, "native status mutated state")
            metadata = box.device / "release.json"
            metadata.write_text('{"format":1,"current":"0.2.0","previous":"0.1.0"}')
            metadata.chmod(0o644)
            set_owner(box.root, metadata, "0:0")
            before = snapshot(box)
            require(native("status")["versions"] == {"current": "0.2.0", "previous": "0.1.0"},
                    "native release versions differ")
            require(snapshot(box) == before, "native version observation mutated state")
            metadata.write_text('{"format":1,"current":"SYNTHETIC-TOKEN","previous":null}')
            require(native("status", code=1)["error"] == "status_failed", "native metadata refusal differs")
            metadata.unlink()
            (box.root / "running").touch()
            doctor_tools(box)
            checks = native("doctor")["checks"]
            require([check["name"] for check in checks] == [
                "adb", "device", "hassmic", "egress", "clock", "disk_free", "process_user"],
                "native doctor checks differ")
            require(all(check["ok"] and check["fix"] is None for check in checks), "native healthy doctor failed")
            result = native("status")
            require(result["process_user"] == "nobody" and result["ok"], "native identity differs")
            (box.root / "process-user").write_text("root")
            result = native("status", code=1)
            require(result["last_error"] == "unsafe_process_user", "native root refusal differs")
            (box.root / "running").unlink()
            (box.root / "process-user").unlink()
            result = native("uninstall", *(["--purge"] if purge else []))
            require(result["ok"] and result["purged"] == purge, "native uninstall result differs")
            require(not (box.device / "bin").exists(), "native uninstall retained binaries")
            if purge:
                require(not any((box.device / name).exists() for name in ("private", "home")),
                        "native purge retained state")
            else:
                retained(box, values)
            require((box.root / "remote/hassmic/runas").read_bytes() == hassmic, "native changed hassmic")
    print("echo-setup native shell: status=PASS identity=PASS uninstall=PASS retention=PASS privacy=PASS")


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

        def install(version):
            result = subprocess.run(["/bin/sh", str(bundle / "scripts/echo-install.sh"),
                                     "--device", "install", "--bundle", str(bundle),
                                     "--release-version", version], env=env,
                                    capture_output=True, timeout=15)
            require(result.returncode == 0, "release install failed: " + result.stderr.decode())

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
    with Sandbox() as box:
        box.install()
        original = public_bytes(box)
        box.files["bridge"].write_text("version B")
        (box.root / "fault").write_text("push")
        box.install(code=1)
        require((box.device / "releases/pending").is_file(), "partial install is not marked")
        box.run(["status"], 1)
        box.run(["start"], 1)
        box.run(["rollback"])
        require(public_bytes(box) == original, "failed update lost recoverable files")
        box.files["bridge"].write_text("version C")
        box.install()
        require((box.device / "bin/echo-bridge").read_text() == "version C", "recovery did not install")
    for kind in ("symlink", "hardlink", "fifo", "owner", "mode"):
        with Sandbox() as box:
            box.install()
            box.install()
            original = public_bytes(box)
            target = box.device / "releases/previous/bin/echo-bridge"
            outside = box.root / "outside"
            outside.write_text("outside bytes")
            if kind in ("symlink", "hardlink", "fifo"):
                target.unlink()
                if kind == "symlink": target.symlink_to(outside)
                elif kind == "hardlink": os.link(outside, target)
                else: os.mkfifo(target)
            elif kind == "owner": set_owner(box.root, target, "9999:9999")
            else: target.chmod(0o666)
            box.run(["rollback"], 1)
            require(public_bytes(box) == original and outside.read_text() == "outside bytes",
                    "unsafe snapshot changed active or outside bytes")
            require(not (box.root / "remote/echo-bridge.lock").exists(), "unsafe snapshot took lock")
    print("echo-install releases: snapshot=PASS rollback=PASS partial=PASS retention=PASS")
