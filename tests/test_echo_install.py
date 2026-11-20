"""T3 component CLI witness: real POSIX shell, recording adb, no device access."""
import os
from pathlib import Path
import sys
import errno
import stat
ROOT = Path(__file__).resolve().parents[1]
DEVICE = "/data/local/echo-bridge"
SENTINEL = "synthetic-private-content-do-not-print"
def require(condition, message):
    if not condition:
        raise AssertionError(message)
def fake_adb():
    """Translate the fixed remote filesystem into a fresh Linux sandbox."""
    import json
    import selectors
    import shutil
    import subprocess
    args = sys.argv[2:]
    base = Path(os.environ["FAKE_ROOT"])
    with (base / "ledger").open("a", encoding="utf-8") as output:
        output.write(json.dumps(args) + "\n")
    if args[:1] == ["-s"]:
        args = args[2:]
    print(SENTINEL, file=sys.stderr)
    fault = base / "fault"
    if fault.exists() and fault.read_text() in " ".join(args):
        fault.unlink()
        print(SENTINEL)
        return 1
    if args == ["devices"]:
        print("List of devices attached\n" + (base / "devices").read_text())
        return 0
    if args[0] == "push":
        destination = Path(args[2].replace("/data/local", str(base / "remote")))
        shutil.copyfile(args[1], destination)
        set_owner(base, destination, "0:0")
        print(SENTINEL)
        return 0
    if args[0] == "shell" and len(args) == 2:
        command = args[1]
        script = sys.stdin.read() if command.startswith("sh -s -- --device ") else None
        if script is not None:
            script = script.replace("/system/bin/sh", "/bin/sh")
            script = script.replace("/data/local", str(base / "remote"))
            script = script.replace("/proc/", str(base / "proc") + "/")
        command = command.replace("/system/bin/sh", "/bin/sh")
        command = command.replace("/data/local", str(base / "remote"))
        command = command.replace("/proc/", str(base / "proc") + "/")
        pause = base / "pause"
        if pause.exists() and pause.read_text() in command:
            pause.unlink()
            with (base / "paused").open("w") as ready:
                ready.write("paused")
            with (base / "release").open("r") as release:
                release.read()
        result = subprocess.run(["/bin/sh", "-c", command],
                                env=dict(os.environ, PATH=str(base / "remote-tools") + ":/usr/bin:/bin"),
                                input=script.encode() if script is not None else None,
                                capture_output=True, timeout=5)
        if result.returncode:
            # Fixed labels only: never expose remote commands or private stderr.
            stage = next((label for text, label in (
                ("find ", "migration"), ("mv -f", "publication"),
                ("mkdir", "directory"), ("ps -A", "process"),
                ("chmod", "mode")) if text in command), "other")
            (base / "failed-stage").write_text(stage)
        if "setsid " in command and result.returncode == 0:
            # Readiness comes from the real child, not a scheduling sleep.
            fd = os.open(base / "ready", os.O_RDWR | os.O_NONBLOCK)
            with selectors.DefaultSelector() as selector:
                selector.register(fd, selectors.EVENT_READ)
                require(bool(selector.select(4)), "fixture did not announce readiness")
                os.read(fd, 100)
            os.close(fd)
        sys.stdout.buffer.write(result.stdout)
        sys.stderr.buffer.write(result.stderr)
        return result.returncode
    return 1
class Sandbox:
    def __init__(self):
        import shlex
        import shutil
        import tempfile
        self.temp = tempfile.TemporaryDirectory(prefix="echo-install-")
        self.root = Path(self.temp.name)
        self.device = self.root / "remote/echo-bridge"
        scripts = self.root / "scripts"
        scripts.mkdir()
        self.script = scripts / "echo-install.sh"
        shutil.copyfile(ROOT / "scripts/echo-install.sh", self.script)
        for name in ("echo-install-release.sh", "echo-install-setup.sh"):
            shutil.copyfile(ROOT / "scripts" / name, scripts / name)
        (self.root / "remote-tools").mkdir()
        # Ordinary metadata reads and root mode changes can use the host tools
        # directly. Projected ownership and nobody's access still go through the fake.
        passthrough = {
            "stat": '''if [ "${FAKE_UID:-0}" != 9999 ]; then
  case "$2" in %u*) ;; *) exec /usr/bin/stat "$@" ;; esac
fi
''',
            "chmod": 'if [ "${FAKE_UID:-0}" != 9999 ]; then exec /usr/bin/chmod "$@"; fi\n',
        }
        for name in ("ps", "stat", "chown", "chmod"):
            wrapper = self.root / "remote-tools" / name
            wrapper.write_text("#!/bin/sh\n" + passthrough.get(name, "")
                               + "exec " + shlex.quote(sys.executable) + " -S "
                               + shlex.quote(str(Path(__file__).resolve())) + " --tool " + name + ' "$@"\n')
            wrapper.chmod(0o755)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        # A closed caller PATH cannot resolve any real adb, even on a developer host.
        for tool in ("dirname", "cat", "awk", "mktemp", "rm"):
            executable = shutil.which(tool)
            require(executable is not None, "required Linux utility absent")
            wrapper = self.tools / tool
            wrapper.write_text("#!/bin/sh\nexec " + shlex.quote(executable) + ' "$@"\n')
            wrapper.chmod(0o755)
        adb = self.tools / "adb"
        adb.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " -S "
                       + shlex.quote(str(Path(__file__).resolve())) + ' --fake-adb "$@"\n')
        adb.chmod(0o755)
        runas = self.root / "remote/hassmic/runas"
        runas.parent.mkdir(parents=True)
        runas.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " -S "
                         + shlex.quote(str(Path(__file__).resolve())) + ' --tool runas "$@"\n')
        runas.chmod(0o755)
        (self.root / "devices").write_text("fixture\tdevice\n")
        self.files = {}
        # Hostile local filenames must stay literal across the adb argv boundary.
        for name in ("config", "bridge", "codex", "ca"):
            path = self.root / (name + " ' $(touch escaped).input")
            path.write_text(SENTINEL + name)
            self.files[name] = path
        self.env = dict(os.environ, PATH=str(self.tools), FAKE_ROOT=str(self.root))
        self.env.pop("CARGO_TARGET_DIR", None)
        self.env.pop("CODEX_ARMV7_OUTPUT_DIR", None)
    def __enter__(self):
        return self
    def __exit__(self, *unused):
        # Only synthetic fixture children can be signalled by test teardown.
        import signal
        for name in ("holder-pid", "child-pid"):
            path = self.root / name
            if path.exists():
                try:
                    pid = int(path.read_text())
                    if name == "holder-pid" and os.getsid(pid) == pid:
                        # Include the launcher's FIFO writer on failed/stubborn runs.
                        os.killpg(pid, signal.SIGKILL)
                    else:
                        os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self.temp.cleanup()
    def calls(self):
        import json
        ledger = self.root / "ledger"
        return [json.loads(line) for line in ledger.read_text().splitlines()] if ledger.exists() else []
    def run(self, args, code=0):
        import subprocess
        result = subprocess.run(["/bin/sh", str(self.script), *args], cwd=self.root,
                                env=self.env, capture_output=True, text=True, timeout=15)
        stage = self.root / "failed-stage"
        require(result.returncode == code, "CLI exit differs (stage="
                + (stage.read_text() if stage.exists() else "unavailable") + "): " + result.stderr)
        require(SENTINEL not in result.stdout + result.stderr, "private bytes leaked")
        require(not (self.root / "escaped").exists(), "local input executed")
        if code:
            require("echo-install: installed" not in result.stdout, "false success")
        return result
    def install_args(self, force=False):
        args = ["install"]
        for name, path in self.files.items():
            args.extend(["--" + name, str(path)])
        return args + (["--force"] if force else [])
    def install(self, force=False, code=0):
        return self.run(self.install_args(force), code)
    def prompt(self, name, text):
        path = self.root / "examples/prompts" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
def cli_cases():
    with Sandbox() as box:
        (box.tools / "adb").unlink()
        result = box.run(["--help"])
        require("--config FILE" in result.stdout and "Exit status: 0" in result.stdout, "help incomplete")
        for args in ([], ["--help", "extra"], ["install", "--config"],
                     ["install", "--other", SENTINEL], ["start", "--config", SENTINEL],
                     ["install", "--force", "--force"], ["install", "--purge"],
                     ["install", "--config", "x", "--config", "y"]):
            result = box.run(args, 2)
            require(result.stderr == "echo-install: invalid usage; see --help\n", "usage output differs")
        box.install(code=1)
        require(box.calls() == [], "parser or missing adb invoked device")
    with Sandbox() as box:
        box.files["bridge"].unlink()
        box.install(code=1)
        require(box.calls() == [], "missing input touched device")
    print("F01 F02: PASS")
def prerequisite_cases():
    for devices in ("", "fixture-a\tdevice\nfixture-b\tdevice\n", "fixture\toffline\n",
                    "fixture\tunauthorized\n"):
        with Sandbox() as box:
            (box.root / "devices").write_text(devices)
            result = box.install(code=1)
            require("fixture" not in result.stdout + result.stderr, "device identifier leaked")
            require(box.calls() == [["devices"]], "invalid selection caused effects")
    with Sandbox() as box:
        (box.root / "remote/hassmic/runas").unlink()
        box.install(code=1)
        require(len(box.calls()) == 2 and not box.device.exists(), "missing hassmic caused effects")
    print("F03 F04: PASS")
def modes_and_defaults():
    import shutil
    for defaults in (False, True):
        with Sandbox() as box:
            box.prompt("voice.md", SENTINEL + "voice")
            if defaults:
                target = box.root / "target/armv7-linux-androideabi/release/echo-bridge"
                target.parent.mkdir(parents=True)
                shutil.copyfile(box.files["bridge"], target)
                output = box.root / "codex-output"
                output.mkdir()
                shutil.copyfile(box.files["codex"], output / "codex-app-server")
                shutil.copyfile(box.files["ca"], output / "cacert.pem")
                box.env["CODEX_ARMV7_OUTPUT_DIR"] = str(output)
                result = box.run(["install", "--config", str(box.files["config"])])
            else:
                result = box.install()
            require(result.stdout == "echo-install: installed\n" and not result.stderr, "install output differs")
            modes = {"": 0o755, "bin": 0o755, "private": 0o700, "home": 0o700,
                     "bin/echo-bridge": 0o755, "bin/codex-app-server": 0o755,
                     "bin/cacert.pem": 0o644, "private/bridge.toml": 0o600,
                     "private/launch.sh": 0o700, "private/bridge.log": 0o600}
            for name, mode in modes.items():
                require((box.device / name).stat().st_mode & 0o777 == mode, "installed mode differs")
            for name, remote in (("bridge", "bin/echo-bridge"), ("codex", "bin/codex-app-server"),
                                 ("ca", "bin/cacert.pem"), ("config", "private/bridge.toml")):
                require((box.device / remote).read_bytes() == box.files[name].read_bytes(), "installed bytes differ")
            pushes = [call for call in box.calls() if call[0] == "push"]
            require(len(pushes) == 5 and all(call[2] == DEVICE + "/private/.install-stage" for call in pushes),
                    "push destinations or count differ")
            require(not (box.root / "remote/echo-bridge.lock").exists(), "lock leaked")
    print("F05: PASS")
def retention_cases():
    with Sandbox() as box:
        box.prompt("voice.md", "default voice")
        box.install()
        (box.device / "home/prompts/voice.md").write_text("owner voice")
        (box.device / "home/settings.toml").write_text("owner settings")
        (box.device / "private/credential").write_text("owner secret")
        box.files["config"].write_text("replacement config")
        box.prompt("runner.md", "default runner")
        box.install()
        require((box.device / "private/bridge.toml").read_text() == SENTINEL + "config", "config overwritten")
        box.install(force=True)
        require((box.device / "private/bridge.toml").read_text() == "replacement config", "force ineffective")
        for name, text in (("home/prompts/voice.md", "owner voice"), ("home/settings.toml", "owner settings"),
                           ("home/prompts/runner.md", "default runner"), ("private/credential", "owner secret")):
            require((box.device / name).read_text() == text, "owner state changed")
    for empty in (False, True):
        with Sandbox() as box:
            if empty:
                (box.root / "examples/prompts").mkdir(parents=True)
            result = box.install()
            require(result.stdout == "echo-install: prompt defaults absent; skipped\necho-install: installed\n",
                    "missing prompts output differs")
            require(len([call for call in box.calls() if call[0] == "push"]) == 4, "empty glob pushed")
    print("F06 F07: PASS")

def failure_cases():
    from test_echo_install_lifecycle import recovery_case
    for label, fault in (("push", "push"), ("chmod", "chmod '600'"),
                         ("rename", "mv -f"), ("observation", "ps -A")):
        with Sandbox() as box:
            try:
                recovery_case(box, fault)
            except AssertionError as error:
                raise AssertionError("F08 recovery case=" + label + ": " + str(error)) from error
        print("F08 recovery=" + label + ": PASS", flush=True)
    with Sandbox() as box:
        outside = box.root / "outside"
        outside.mkdir()
        box.device.symlink_to(outside, target_is_directory=True)
        box.install(code=1)
        require(list(outside.iterdir()) == [], "symlink escaped root")
    with Sandbox() as box:
        lock = box.root / "remote/echo-bridge.lock"
        lock.mkdir()
        box.install(code=1)
        require(lock.is_dir() and not box.device.exists(), "busy lock mutated state")
    print("F08: PASS")

def ownership_attribute(path):
    info = path.stat()
    if stat.S_ISFIFO(info.st_mode):
        # Linux forbids user xattrs on FIFOs. Keep their generation-specific
        # metadata on the parent directory, which supports user xattrs.
        return path.parent, f"user.echo_fifo_{info.st_ino}_{info.st_ctime_ns}"
    return path, "user.echo_install_owner"

def owner(base, path):
    target, attribute = ownership_attribute(path)
    try:
        return os.getxattr(target, attribute).decode("ascii")
    except OSError as error:
        if error.errno != errno.ENODATA:
            raise
        return "0:0"

def set_owner(base, path, value):
    # Follow the filesystem object's lifetime: rename/hardlink retain ownership,
    # deletion destroys it, and inode reuse cannot inherit a deleted owner.
    target, attribute = ownership_attribute(path)
    os.setxattr(target, attribute, value.encode("ascii"))

def metadata_tool(base, name, args):
    if name == "runas":
        require(args[:2] == ["nobody", "3990,inet"], "wrong launch identity")
        # The private script and its inputs must be accessible before execution.
        for path in (base / "remote/echo-bridge/private").rglob("*"):
            require(owner(base, path).split(":")[0] == "9999", "nobody cannot access root-owned state")
        if len(args) > 3 and args[3] == str(base / "remote/echo-bridge/private/launch.sh"):
            from test_echo_install_lifecycle import prepared_run
            prepared_run(base)
            if (base / "synthetic-bridge").exists():
                # Keep installed ARM bytes intact; substitute only the executable edge.
                launcher = Path(args[3]).read_text()
                binary = str(base / "remote/echo-bridge/bin/echo-bridge") + " --config "
                require(launcher.count(binary) == 1, "unexpected executable boundary")
                args = [*args[:3], "-c", launcher.replace(binary, str(base / "synthetic-bridge") + " --config ")]
        (base / "runas-user").write_text("nobody")
        os.execvpe(args[2], args[2:], dict(os.environ, FAKE_UID="9999"))
    path = Path(args[-1])
    if name in ("stat", "chmod") and os.environ.get("FAKE_UID") == "9999":
        require(owner(base, path).split(":")[0] == "9999", "nobody access denied")
    if name == "chown":
        require(os.environ.get("FAKE_UID", "0") == "0", "nonroot chown")
        operands = [arg for arg in args if not arg.startswith("-")]
        value = operands[0]
        for name in operands[1:]:
            target = Path(name)
            for entry in [target, *target.rglob("*")] if "-R" in args else [target]:
                set_owner(base, entry, value)
            with (base / "ownership-ledger").open("a") as output:
                output.write(str(target) + "=" + value + "\n")
        return 0
    if name == "stat" and args[1] in ("%u", "%u:%g:%a", "%u:%h:%a", "%u:%a"):
        for argument in args[2:]:
            path = Path(argument)
            value, info = owner(base, path), path.stat()
            if args[1] == "%u": print(value.split(":")[0])
            elif args[1] == "%u:%h:%a":
                print(value.split(":")[0] + ":" + str(info.st_nlink) + ":" + format(info.st_mode & 0o777, "o"))
            elif args[1] == "%u:%a": print(value.split(":")[0] + ":" + format(info.st_mode & 0o777, "o"))
            else: print(value + ":" + format(info.st_mode & 0o777, "o"))
        return 0
    os.execv("/usr/bin/" + name, [name, *args])

def remote_tool():
    base = Path(os.environ["FAKE_ROOT"])
    name = sys.argv[2]
    if name in ("runas", "chown", "stat", "chmod"):
        return metadata_tool(base, name, sys.argv[3:])
    if name == "ps":
        user_mode = "PID,USER,NAME" in sys.argv
        user = (base / "process-user").read_text() if (base / "process-user").exists() else "nobody"
        print("PID USER NAME" if user_mode else "PID NAME\n12 sh echo-bridge launcher")
        path = base / "child-pid"
        if (base / "running").exists():
            print("13 " + (user + " " if user_mode else "") + "echo-bridge")
        elif path.exists() and (base / "mode").read_text() != "hidden":
            try:
                state = Path("/proc") / path.read_text() / "stat"
                if state.read_text().split(") ")[1].split()[0] != "Z":
                    print(path.read_text() + " " + (user + " " if user_mode else "") + "echo-bridge")
                    if user_mode and (base / "mode").read_text() == "stale":
                        (base / "stale-observed").touch()
            except FileNotFoundError:
                pass
    elif name == "date":
        clock = base / "clock"
        value = int(clock.read_text()) if clock.exists() else 0
        if (base / "mode").read_text() == "deadline" and value == 1:
            value = 20
        clock.write_text(str(value + 1))
        print(value)
    elif name == "sleep":
        # Advance the virtual deadline only after a failed, observed stale poll.
        # The nobody FIFO-holder and stop cleanup must not advance this clock.
        run = base / "remote/echo-bridge/private/run"
        if (os.environ.get("FAKE_UID") != "9999" and (base / "stale-observed").exists()
                and not (base / "stale-expired").exists() and not (run / "stop").exists()):
            clock = base / "clock"
            clock.write_text(str(int(clock.read_text()) + 20))
            (base / "stale-expired").touch()
    return 0

def prepare_lifecycle(box, mode="ready"):
    from test_echo_install_lifecycle import configure_lifecycle
    configure_lifecycle(box, mode)
    box.install()

def setup_wake_cases():
    # Real shell observer, synthetic log only; each case has its own filesystem.
    old = SENTINEL + "\nwake: COMPUTER type=2\nclient connected / satellite: running\n"
    cases = {"stale": ("", "pending", "pending"),
             "wake": ("wake: COMPUTER type=2\n", "confirmed", "pending"),
             "link": ("satellite: running\n", "pending", "pending"),
             "wrong-type": ("wake: COMPUTER type=20\nsatellite: running\n", "pending", "pending"),
             "reversed": ("satellite: running\nwake: COMPUTER type=2\n", "confirmed", "pending"),
             "partial": ("wake: COMPUTER type=2\nsatellite: running", "pending", "pending"),
             "fresh": (old, "confirmed", "connected"),
             "disconnected": (old + "client disconnected\n", "confirmed", "pending"),
             "partial-disconnect": (old + "client disconn", "pending", "pending")}
    for kind in (*cases, "rotated", "truncated", "rewritten", "symlink", "parent-link", "injection"):
        with Sandbox() as box:
            for path in box.script.parent.glob("echo-install*.sh"):
                path.write_text(path.read_text().replace("/data/local", str(box.root / "remote")))
            box.env["PATH"] = str(box.root / "remote-tools") + ":/usr/bin:/bin"
            log = box.root / "remote/hassmic/boot.log"
            log.write_text(old)
            box.run(["wake-cursor"], 2)
            cursor = box.run(["--device", "wake-cursor"]).stdout.strip()
            fields = cursor.split(":")
            require(len(fields) == 4 and int(fields[2]) == len(old), "cursor differs")
            require(fields[3] == __import__("hashlib").sha256(old.encode()).hexdigest(), "prefix differs")
            if kind in cases: log.write_text(old + cases[kind][0])
            elif kind == "rotated": log.rename(log.with_suffix(".old")); log.write_text(old + old)
            elif kind == "truncated": log.write_text("")
            elif kind == "rewritten": log.write_text("x" * len(old) + old)
            elif kind == "injection": cursor = "1:1:0:$(touch escaped)"
            else:
                target = log if kind == "symlink" else log.parent
                saved = target.with_name(target.name + "-saved")
                target.rename(saved)
                target.symlink_to(saved, target_is_directory=kind == "parent-link")
            result = box.run(["--device", "wake-check", cursor], 0 if kind in cases else 1)
            expected = ("wake=" + cases[kind][1] + "\nlink=" + cases[kind][2] + "\n") if kind in cases else ""
            require(result.stdout == expected and not result.stderr, "wake observation/privacy differs")
            require(not (box.root / "remote/echo-bridge.lock").exists(), "observation took lifecycle lock")
    with Sandbox() as box:
        for path in box.script.parent.glob("echo-install*.sh"):
            path.write_text(path.read_text().replace("/data/local", str(box.root / "remote")))
        box.env["PATH"] = "/usr/bin:/bin"
        log = box.root / "remote/hassmic/boot.log"
        log.write_text("wake: COMPUTER type=")
        box.run(["--device", "wake-cursor"], 1)
        log.write_text("")
        require(":0:" in box.run(["--device", "wake-cursor"]).stdout, "empty cursor refused")
    print("echo-install wake: fresh=PASS stale=PASS rotation=PASS partial=PASS privacy=PASS")
def main():
    from concurrent.futures import ThreadPoolExecutor
    require(sys.platform.startswith("linux"), "Linux is required for this runtime witness")
    from test_echo_install_lifecycle import lifecycle_cases, nonroot_cases, ownership_lifetime_case, setup_operator_cases
    from test_echo_install_release import release_cases
    from test_echo_install_status import status_cases
    import time as _t
    def _timed(function):
        start = _t.time(); function(); print(f"CASE-TIMING {function.__name__} {_t.time() - start:.1f}", flush=True)
    _timed(ownership_lifetime_case)
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(_timed,
                      (cli_cases, prerequisite_cases, modes_and_defaults, retention_cases,
                       failure_cases, release_cases, lifecycle_cases, status_cases, nonroot_cases,
                       setup_operator_cases, setup_wake_cases)))
    print("echo-install tests: install=PASS lifecycle=PASS status=PASS retention=PASS privacy=PASS")

if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--native-lifecycle":
        from test_echo_install_status import native_lifecycle_cases
        native_lifecycle_cases(sys.argv[2])
        sys.exit(0)
    if sys.argv[1:] == ["--device-transport"]:
        from test_echo_install_release import device_transport_cases
        device_transport_cases()
        sys.exit(0)
    if len(sys.argv) > 1 and sys.argv[1] == "--tool":
        sys.exit(remote_tool())
    if sys.argv[1:] == ["--fake-adb"] or (len(sys.argv) > 1 and sys.argv[1] == "--fake-adb"):
        sys.exit(fake_adb())
    main()
