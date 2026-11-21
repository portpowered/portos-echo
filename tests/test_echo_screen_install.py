"""T3 component CLI witness: real installer and prebuilt fake adb, no device/network."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SH = shutil.which("sh")
FAKE = ROOT / "target" / "debug" / "echo-lab-fake-adb"
HOME = ["shell", "printf '%s\\n' org.portos.echoscreen > /data/local/hassmic/state/screen_home && chown system:system /data/local/hassmic/state/screen_home && chmod 644 /data/local/hassmic/state/screen_home"]
START = ["shell", "am", "start", "-n", "org.portos.echoscreen/.MainActivity"]
GRANT = ["shell", "pm", "grant", "org.portos.echoscreen", "android.permission.CAMERA"]
REMOVE = ["shell", "rm", "-f", "/data/local/hassmic/state/screen_home"]
UNINSTALL = ["uninstall", "org.portos.echoscreen"]
USAGE = ("Usage: sh scripts/echo-screen-install.sh [--url <page-url>]\n"
         "       sh scripts/echo-screen-install.sh --uninstall\n")
# Deliberately fake; never print token-bearing argv, even on assertion failure.
URL = "http://localhost:4173/?events=synthetic+a&actions=b%25;'$(>sentinel)`>sentinel`#片"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def quote(value):
    return "'" + value.replace("'", "'\\''") + "'"


def case(args=(), fail_step=None, missing=None, launch_error=False):
    with tempfile.TemporaryDirectory(prefix="echo-screen-install-") as temporary:
        root = Path(temporary)
        scripts = root / "scripts"
        scripts.mkdir()
        shutil.copyfile(ROOT / "scripts/echo-screen-install.sh", scripts / "echo-screen-install.sh")
        (scripts / "echo-screen-install.sh").chmod(0o755)
        apk = root / "target" / "echo-screen" / "echo-screen.apk"
        apk.parent.mkdir(parents=True)
        if missing != "apk" and args != ("--uninstall",):
            apk.write_bytes(b"synthetic APK for CLI only")
        tools = root / "bin"
        tools.mkdir()
        # A closed PATH: no possibility of falling back to an installed real adb.
        for name in ("dirname", "sed", "mktemp", "rm", "grep"):
            tool = shutil.which(name)
            if tool:
                wrapper = tools / name
                wrapper.write_text("#!/bin/sh\nexec " + quote(Path(tool).as_posix()) + ' "$@"\n', encoding="utf-8")
                wrapper.chmod(0o755)
        if missing != "adb":
            shutil.copyfile(FAKE, tools / "adb")
            (tools / "adb").chmod(0o755)
        ledger = root / "ledger.jsonl"
        fixture = root / "fixture.json"
        install = ["install", "-r", apk.as_posix()]
        sequence = ([REMOVE, UNINSTALL] if args == ("--uninstall",) else
                    [install, GRANT, HOME, START + (["--es", "url", quote(args[1])] if len(args) == 2 else [])])
        responses = {}
        for index, argv in enumerate(sequence):
            responses[" ".join(argv)] = {
                "stdout": json.dumps(argv) + "\n" + URL + "\n",
                "stderr": ("Error: " if launch_error and argv[:len(START)] == START else "") + URL + "\n",
                "code": 1 if fail_step == index else 0,
            }
        fixture.write_text(json.dumps(responses), encoding="utf-8")
        env = dict(os.environ, PATH=str(tools), ECHO_LAB_FAKE_LEDGER=str(ledger),
                   ECHO_LAB_FAKE_FIXTURE=str(fixture))
        # Keep hostile argument bytes literal through the caller's quoted argv.
        env["ECHO_SCREEN_TEST_SCRIPT"] = (scripts / "echo-screen-install.sh").as_posix()
        command = 'exec "$ECHO_SCREEN_TEST_SCRIPT"'
        for index, argument in enumerate(args):
            env[f"CASE_ARG_{index}"] = argument
            command += f' "$CASE_ARG_{index}"'
        result = subprocess.run([SH, "-c", command],
                                env=env, cwd=root, capture_output=True, text=True, timeout=10)
        calls = [json.loads(line) for line in ledger.read_text(encoding="utf-8").splitlines()] if ledger.exists() else []
        if missing:
            expected = "APK is not readable" if missing == "apk" else "adb is required"
            require((result.returncode, result.stdout, result.stderr, calls) ==
                    (1, "", f"echo-screen: {expected}\n", []), "prerequisite result differs")
        elif launch_error:
            require((result.returncode, result.stdout, result.stderr, calls) ==
                    (1, "", "echo-screen: launch failed\n", sequence), "API-28 launch error not redacted")
        elif fail_step is not None:
            diagnostics = (["home removal failed", "uninstall failed"] if args == ("--uninstall",) else
                           ["install failed", "camera permission grant failed", "home setup failed", "launch failed"])
            require((result.returncode, result.stdout, result.stderr) ==
                    (1, "", f"echo-screen: {diagnostics[fail_step]}\n"), "failure output differs or leaked")
            require(calls == sequence[:fail_step + 1], "failure did not stop at exact command")
        elif result.returncode == 2:
            require((result.stdout, result.stderr, calls) == ("", USAGE, []), "usage result differs or invoked adb")
        else:
            output = "echo-screen uninstalled\n" if args == ("--uninstall",) else "echo-screen installed\n"
            require((result.returncode, result.stdout, result.stderr) == (0, output, ""), "success result differs or leaked")
            require(calls == sequence, "exact adb argv differs")
            if len(args) == 2:
                # adb's shell concatenation is reproduced using the real local shell.
                recovered = subprocess.run([SH, "-c", "printf '%s' " + calls[-1][-1]], cwd=root,
                                           capture_output=True, timeout=5)
                require(recovered.returncode == 0 and recovered.stdout.decode("utf-8") == args[1]
                        and recovered.stderr == b"", "remote quoting changed URL")
                require(not (root / "sentinel").exists(), "remote shell executed URL contents")
        return result.returncode


def main():
    require(SH is not None and FAKE.is_file(), "build the fake adb and provide POSIX sh first")
    jobs = [lambda: case(), lambda: case(("--url", URL)), lambda: case(("--uninstall",)),
            lambda: case(("--url", URL), launch_error=True)]
    for args, count in (((), 4), (("--url", URL), 4), (("--uninstall",), 2)):
        jobs.extend(lambda args=args, step=step: case(args, fail_step=step) for step in range(count))
    for missing in ("apk", "adb"):
        jobs.append(lambda missing=missing: case(missing=missing))
    invalid = [("--other",), ("--url",), ("--url", ""), ("--uninstall", "--url", URL),
               ("--url", URL, "extra"), ("positional",), ("--url", "file:///tmp/page"),
               ("--url", "http:///empty"), ("--url", "http://synthetic@localhost/"),
               ("--url", "http://localhost/\rsecret"), ("--url", "https://localhost/\nsecret")]
    jobs.extend(lambda args=args: require(case(args) == 2, "invalid arguments accepted") for args in invalid)
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda job: job(), jobs))
    print("installer: install=4; install-url=4; uninstall=2; grant-before-launch=true; failure-stops=true; url=[redacted]")


if __name__ == "__main__":
    main()
