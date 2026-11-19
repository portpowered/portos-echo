"""T3 component CLI witness: real POSIX shell, recording adb, no device access."""
import json
import selectors
import signal
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

ROOT = Path(__file__).resolve().parents[1]
DEVICE = "/data/local/echo-bridge"
SENTINEL = "synthetic-private-content-do-not-print"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def fake_adb():
    """Translate the fixed remote filesystem into a fresh Linux sandbox."""
    args = sys.argv[2:]
    base = Path(os.environ["FAKE_ROOT"])
    with (base / "ledger").open("a", encoding="utf-8") as output:
        output.write(json.dumps(args) + "\n")
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
        print(SENTINEL)
        return 0
    if args[0] == "shell" and len(args) == 2:
        command = args[1]
        command = command.replace("/system/bin/sh", "/bin/sh")
        command = command.replace("/data/local", str(base / "remote"))
        pause = base / "pause"
        if pause.exists() and pause.read_text() in command:
            pause.unlink()
            with (base / "paused").open("w") as ready:
                ready.write("paused")
            with (base / "release").open("r") as release:
                release.read()
        result = subprocess.run(["/bin/sh", "-c", command],
                                env=dict(os.environ, PATH=str(base / "remote-tools") + ":/usr/bin:/bin"),
                                capture_output=True, timeout=5)
        if "setsid /bin/sh" in command and result.returncode == 0:
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
        self.temp = tempfile.TemporaryDirectory(prefix="echo-install-")
        self.root = Path(self.temp.name)
        self.device = self.root / "remote/echo-bridge"
        scripts = self.root / "scripts"
        scripts.mkdir()
        self.script = scripts / "echo-install.sh"
        shutil.copyfile(ROOT / "scripts/echo-install.sh", self.script)
        (self.root / "remote-tools").mkdir()
        ps = self.root / "remote-tools/ps"
        ps.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " "
                      + shlex.quote(str(Path(__file__).resolve())) + ' --tool ps "$@"\n')
        ps.chmod(0o755)
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
        adb.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " "
                       + shlex.quote(str(Path(__file__).resolve())) + ' --fake-adb "$@"\n')
        adb.chmod(0o755)
        runas = self.root / "remote/hassmic/runas"
        runas.parent.mkdir(parents=True)
        runas.write_text("synthetic runas")
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
        for name in ("child-pid", "holder-pid"):
            path = self.root / name
            if path.exists():
                try:
                    os.kill(int(path.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self.temp.cleanup()

    def calls(self):
        ledger = self.root / "ledger"
        return [json.loads(line) for line in ledger.read_text().splitlines()] if ledger.exists() else []

    def run(self, args, code=0):
        result = subprocess.run(["/bin/sh", str(self.script), *args], cwd=self.root,
                                env=self.env, capture_output=True, text=True, timeout=15)
        require(result.returncode == code, "CLI exit differs: " + result.stderr)
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
    for fault in ("push", "chmod '600'", "mv -f", "ps -A"):
        with Sandbox() as box:
            box.install()
            original = (box.device / "private/bridge.toml").read_bytes()
            box.files["config"].write_text("new config")
            (box.root / "fault").write_text(fault)
            box.install(force=True, code=1)
            require((box.device / "private/bridge.toml").read_bytes() == original, "failed stage lost config")
            require(not (box.device / "private/.install-stage").exists(), "stage leaked")
            require(not (box.root / "remote/echo-bridge.lock").exists(), "failed operation leaked lock")
            box.install(force=True)
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



def remote_tool():
    base = Path(os.environ["FAKE_ROOT"])
    name = sys.argv[2]
    if name == "ps":
        print("PID NAME\n12 sh echo-bridge launcher")
        path = base / "child-pid"
        if (base / "running").exists():
            print("13 echo-bridge")
        elif path.exists() and (base / "mode").read_text() != "hidden":
            try:
                state = Path("/proc") / path.read_text() / "stat"
                if state.read_text().split(") ")[1].split()[0] != "Z":
                    print(path.read_text() + " echo-bridge")
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
        pass  # Production polling is driven by the synthetic date clock.
    return 0


def prepare_lifecycle(box, mode="ready"):
    os.mkfifo(box.root / "ready")
    runas = box.root / "remote/hassmic/runas"
    runas.write_text('#!/bin/sh\n[ "$1" = root ] && [ "$2" = 3990,inet ] || exit 1\nshift 2\nexec "$@"\n')
    (box.root / "remote/hassmic/boot.log").write_text("old client connected\n")
    fixture = """import os, sys, signal, json
from pathlib import Path
base = Path(os.environ['FAKE_ROOT'])
(base / 'child-pid').write_text(str(os.getpid()))
(base / 'holder-pid').write_text(str(os.getppid()))
(base / 'child-args').write_text(json.dumps(sys.argv[1:]))
(base / 'child-session').write_text(str(os.getsid(0)))
mode = (base / 'mode').read_text()
log = base / 'remote/hassmic/boot.log'
if mode in ('ready', 'hidden', 'deadline', 'stubborn'):
    with log.open('a') as output: output.write('client connected / satellite: running\\n')
elif mode == 'rotate':
    log.rename(log.with_suffix('.old'))
    log.write_text('client connected\\n')
elif mode == 'truncate':
    log.write_text('client connected\\n')
with (base / 'ready').open('w') as ready: ready.write('ready')
if mode != 'exit':
    sys.stdin.buffer.read()
    (base / 'eof').write_text('EOF')
    if mode == 'stubborn': signal.pause()
"""
    box.files["bridge"].write_text("#!" + sys.executable + "\n" + fixture)
    (box.root / "mode").write_text(mode)
    for name in ("ps", "date", "sleep"):
        wrapper = box.root / "remote-tools" / name
        wrapper.write_text("#!/bin/sh\nexec " + shlex.quote(sys.executable) + " "
                           + shlex.quote(str(Path(__file__).resolve())) + " --tool " + name + ' "$@"\n')
        wrapper.chmod(0o755)
    box.install()



def main():
    require(sys.platform.startswith("linux"), "Linux is required for this runtime witness")
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda function: function(),
                      (cli_cases, prerequisite_cases, modes_and_defaults, retention_cases, failure_cases)))
    from test_echo_install_lifecycle import lifecycle_cases
    lifecycle_cases()
    print("echo-install tests: install=PASS lifecycle=PASS retention=PASS privacy=PASS; status pending")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--tool":
        sys.exit(remote_tool())
    if sys.argv[1:] == ["--fake-adb"] or (len(sys.argv) > 1 and sys.argv[1] == "--fake-adb"):
        sys.exit(fake_adb())
    main()
