"""Read-only status, retention and complete CLI journey against recording adb."""
import shutil
import os
import selectors
import json
import subprocess

from test_echo_install import Sandbox, prepare_lifecycle, require


def owner_state(box):
    values = {"private/bridge.toml": "owner config", "private/credential": "owner private",
              "home/prompts/voice.md": "owner voice", "home/settings.toml": "owner settings"}
    for name, value in values.items():
        (box.device / name).write_text(value)
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
        require(result.stdout == "process=running\nprocess_user=nobody\nuptime_seconds=42\n"
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
                require(result.returncode == code, "native lifecycle exit differs: " + command)
                for marker in ("synthetic-private-content-do-not-print", str(box.root)):
                    require(marker not in result.stdout + result.stderr, "native output leaked private data")
                return json.loads(result.stdout)

            result = native("status")
            require(result["process"] == "stopped" and result["process_user"] == "unavailable",
                    "native stopped observation differs")
            require(result["versions"] == {"current": None, "previous": None}, "invented versions")
            require(snapshot(box) == before, "native status mutated state")
            (box.root / "running").touch()
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
