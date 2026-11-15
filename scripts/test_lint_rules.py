"""T0 real-tool regressions through make lint-rules; never emit captured secrets.

Run explicitly with python scripts/test_lint_rules.py. Each invocation owns one
offline local clone and reuses it for clean, staged, untracked, recovery and
invalid-config cases. No scanner, Cargo or Make substitutes are used.
"""

import os
from pathlib import Path
import random
import signal
import string
import subprocess
import tempfile
import time


SEED = "m0-seed-fake-aws.txt"
RULE = "aws-access-token"


class GateFailure(Exception):
    """Only fixed, credential-free diagnostics may leave the runner."""


def run(command, root, environment, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise GateFailure("verification budget exhausted")
    options = {"start_new_session": True} if os.name != "nt" else {
        "creationflags": subprocess.CREATE_NEW_PROCESS_GROUP,
    }
    with subprocess.Popen(command, cwd=root, env=environment,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          **options) as process:
        try:
            stdout, stderr = process.communicate(timeout=remaining)
        except BaseException:
            # Make owns shell/Cargo/scanner descendants: reap the entire tree.
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               check=False, timeout=10)
            else:
                os.killpg(process.pid, signal.SIGKILL)
            process.kill()
            process.communicate()
            raise
        return process.returncode, stdout, stderr


def checked(command, root, environment, deadline):
    code, stdout, _ = run(command, root, environment, deadline)
    if code != 0:
        raise GateFailure("fixture Git operation failed")
    return stdout


def gate(root, environment, deadline, expected, secret=None):
    code, stdout, stderr = run(["make", "lint-rules"], root, environment, deadline)
    output = stdout + stderr
    if secret and any(part.encode() in stream for stream in (stdout, stderr)
                      for part in (secret, secret[4:])):
        raise GateFailure("credential redaction failed; captured output suppressed")
    if code != expected:
        raise GateFailure("unexpected make exit; captured output suppressed")
    if secret:
        if not all(label.encode() in output for label in (SEED, RULE, "REDACTED")):
            raise GateFailure("missing redacted path/rule diagnostic")
    elif b"no leaks found" not in output or RULE.encode() in output:
        raise GateFailure("clean scanner control failed")


def exercise(root, environment, deadline):
    gate(root, environment, deadline, 0)
    print("F-1 PASS: clean make lint-rules exit 0")
    secret = "AK" + "IA" + "".join(random.SystemRandom().sample(
        string.ascii_uppercase + "234567", 16))
    seed = root / SEED
    try:
        seed.write_text("aws_access_key_id=" + secret + "\n", encoding="utf-8")
        checked(["git", "add", "--", SEED], root, environment, deadline)
        gate(root, environment, deadline, 2, secret)
        print(f"F-2 PASS: staged seed exit 2; {SEED}; {RULE}; key absent from both streams")
        checked(["git", "reset", "HEAD", "--", SEED], root, environment, deadline)
        gate(root, environment, deadline, 2, secret)
        print(f"F-3 PASS: untracked seed exit 2; {SEED}; {RULE}; key absent from both streams")
    finally:
        # The temporary directory also disposes the index on any interrupted run.
        seed.unlink(missing_ok=True)
    indexed = checked(["git", "ls-files", "--", SEED], root, environment, deadline)
    if seed.exists() or indexed.strip():
        raise GateFailure("seed cleanup failed")
    gate(root, environment, deadline, 0)
    print("F-4 PASS: seed absent from index/filesystem; recovered clean exit 0")
    invalid = dict(environment, GITLEAKS_CONFIG=str(root / "missing-scanner-config.toml"))
    code, stdout, stderr = run(["make", "lint-rules"], root, invalid, deadline)
    output = (stdout + stderr).lower()
    if code == 0 or b"missing-scanner-config.toml" not in output or not any(
            word in output for word in (b"open", b"config", b"read")):
        raise GateFailure("scanner configuration failure did not fail closed")
    print(f"F-5 PASS: invalid scanner configuration fails closed (make exit {code})")


def main():
    deadline = time.monotonic() + 590
    source = Path(__file__).resolve().parent.parent
    environment = dict(os.environ, CARGO_NET_OFFLINE="true")
    try:
        with tempfile.TemporaryDirectory(prefix="echo-secret-gate-") as directory:
            root = Path(directory) / "repo"
            checked(["git", "clone", "--no-hardlinks", "--quiet", str(source), str(root)],
                    source, environment, deadline)
            base = checked(["git", "rev-parse", "origin/main"],
                           source, environment, deadline).decode().strip()
            checked(["git", "update-ref", "refs/remotes/origin/main", base],
                    root, environment, deadline)
            # Permit pre-commit verification without importing other dirty files.
            (root / "Makefile").write_bytes((source / "Makefile").read_bytes())
            exercise(root, environment, deadline)
        return 0
    except GateFailure as error:
        print(f"FAIL: {error}")
    except (OSError, subprocess.SubprocessError):
        print("FAIL: tool unavailable, process timeout or fixture cleanup failure; output suppressed")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
