"""T0 real-tool regressions through make lint-rules; never emit captured secrets.

Run explicitly with python scripts/test_lint_rules.py. Each invocation owns one
offline local clone and reuses it for the S-1 through S-10 source selection,
redaction, recovery and invalid-config cases. No scanner, Cargo or Make substitutes.
"""

import os
from pathlib import Path
import random
import signal
import string
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

import lint_secrets


SEED = "m0-seed-fake-aws.txt"
RULE = "aws-access-token"


class GateFailure(Exception):
    """Only fixed, credential-free diagnostics may leave the runner."""


class SnapshotChecks(unittest.TestCase):
    def test_nul_paths_and_deduplication(self):
        names = ["space é.txt", "line\nbreak.txt", "space é.txt"]
        result = lint_secrets.selected_paths(("\0".join(names) + "\0").encode())
        self.assertEqual(list(result), names[:2])
        with self.assertRaises(ValueError):
            lint_secrets.selected_paths(b"truncated")

    def test_current_content_deletion_and_unsupported_entry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "source"
            target = Path(directory) / "snapshot"
            root.mkdir()
            (root / "file").write_bytes(b"current bytes")
            lint_secrets.copy_source(root, target, "file")
            self.assertEqual((target / "file").read_bytes(), b"current bytes")
            lint_secrets.copy_source(root, target, "deleted")
            self.assertFalse((target / "deleted").exists())
            with self.assertRaises(ValueError):
                lint_secrets.copy_source(root, target, ".")
            with patch.object(Path, "read_bytes", side_effect=PermissionError):
                with self.assertRaises(PermissionError):
                    lint_secrets.copy_source(root, target, "file")

    def test_symlink_file_and_parent_are_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "source"
            target = Path(directory) / "snapshot"
            root.mkdir()
            outside = Path(directory) / "outside"
            outside.mkdir()
            (outside / "file").write_bytes(b"outside content")
            try:
                (root / "link").symlink_to(outside, target_is_directory=True)
                (root / "file-link").symlink_to(outside / "file")
            except OSError:
                self.skipTest("platform does not permit symlink creation")
            lint_secrets.copy_source(root, target, "link/file")
            lint_secrets.copy_source(root, target, "file-link")
            self.assertFalse(target.exists())


def run(command, root, environment, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise GateFailure("verification budget exhausted")
    with subprocess.Popen(command, cwd=root, env=environment,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=remaining)
        except BaseException:
            # Make owns shell/Cargo/scanner descendants: reap the entire tree.
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


def gate(root, environment, deadline, expected, secret=None, path=SEED, finding=True):
    code, stdout, stderr = run(["make", "lint-rules"], root, environment, deadline)
    output = stdout + stderr
    if secret and any(part.encode() in stream for stream in (stdout, stderr)
                      for part in (secret, secret[4:])):
        raise GateFailure("credential redaction failed; captured output suppressed")
    if code != expected:
        raise GateFailure("unexpected make exit; captured output suppressed")
    if secret and finding:
        if not all(label.encode() in output for label in (path, RULE, "REDACTED")):
            raise GateFailure("missing redacted path/rule diagnostic")
    elif b"no leaks found" not in output or RULE.encode() in output:
        raise GateFailure("clean scanner control failed")
    if b"excluded boundary: factory/" not in output:
        raise GateFailure("missing named vendored boundary")


def exercise(root, environment, deadline):
    gate(root, environment, deadline, 0)
    print("S-1 PASS: clean make lint-rules exit 0")
    secret = "AK" + "IA" + "".join(random.SystemRandom().sample(
        string.ascii_uppercase + "234567", 16))
    cases = [("S-2", SEED, True),
             ("S-3", ".claude/m0-seed-allowlist.txt", True),
             ("S-4", "target/m0-seed-allowlist.txt", True),
             ("S-5", SEED, False),
             ("S-7", "factory/m0-seed-fake.txt", True)]
    for case, name, staged in cases:
        seed = root / name
        seed.parent.mkdir(parents=True, exist_ok=True)
        try:
            seed.write_text("aws_access_key_id=" + secret + "\n", encoding="utf-8")
            if staged:
                checked(["git", "add", "-f", "--", name], root, environment, deadline)
            excluded = case == "S-7"
            gate(root, environment, deadline, 0 if excluded else 2,
                 secret, name, finding=not excluded)
            print(f"{case} PASS: make exit {0 if excluded else 2}; {name}; "
                  + ("factory/ boundary" if excluded else f"{RULE}; REDACTED")
                  + "; key/suffix absent from both streams")
        finally:
            checked(["git", "reset", "HEAD", "--", name], root, environment, deadline)
            seed.unlink(missing_ok=True)
        indexed = checked(["git", "ls-files", "--", name], root, environment, deadline)
        if seed.exists() or indexed.strip():
            raise GateFailure("seed cleanup failed")
        gate(root, environment, deadline, 0)

    ignored = [root / "target/scope-output.txt",
               root / ".claude/worktrees/local/scope-output.txt"]
    try:
        for seed in ignored:
            seed.parent.mkdir(parents=True, exist_ok=True)
            seed.write_text("aws_access_key_id=" + secret + "\n", encoding="utf-8")
        gate(root, environment, deadline, 0, secret, finding=False)
        print("S-6 PASS: ignored untracked credential-shaped output; make exit 0")
    finally:
        for seed in ignored:
            seed.unlink(missing_ok=True)

    name = "scripts/scope space é.txt"
    seed = root / name
    try:
        seed.write_text("clean\n", encoding="utf-8")
        checked(["git", "add", "--", name], root, environment, deadline)
        checked(["git", "-c", "user.name=Fixture", "-c",
                 "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
                 "commit", "--quiet", "--only", "-m", "Clean path fixture", "--", name],
                root, environment, deadline)
        seed.write_text("aws_access_key_id=" + secret + "\n", encoding="utf-8")
        gate(root, environment, deadline, 2, secret, name)
        print("S-10 PASS: modified tracked spaced Unicode path; exit 2; REDACTED")
    finally:
        checked(["git", "checkout", "--", name], root, environment, deadline)
    if seed.read_bytes() != b"clean\n":
        raise GateFailure("tracked path recovery failed")
    gate(root, environment, deadline, 0)
    print("S-9 PASS: seeds absent from index/filesystem; recovered clean exit 0")
    invalid = dict(environment, GITLEAKS_CONFIG=str(root / "missing-scanner-config.toml"))
    code, stdout, stderr = run(["make", "lint-rules"], root, invalid, deadline)
    output = (stdout + stderr).lower()
    if code == 0 or b"missing-scanner-config.toml" not in output or not any(
            word in output for word in (b"open", b"config", b"read")):
        raise GateFailure("scanner configuration failure did not fail closed")
    print(f"S-8 PASS: invalid scanner configuration fails closed (make exit {code})")


def main():
    deadline = time.monotonic() + 590
    source = Path(__file__).resolve().parent.parent
    environment = dict(os.environ, CARGO_NET_OFFLINE="true")
    try:
        result = unittest.TextTestRunner(verbosity=1).run(
            unittest.defaultTestLoader.loadTestsFromTestCase(SnapshotChecks))
        if not result.wasSuccessful():
            raise GateFailure("snapshot component checks failed")
        with tempfile.TemporaryDirectory(prefix="echo-secret-gate-") as directory:
            root = Path(directory) / "repo"
            checked(["git", "clone", "--no-hardlinks", "--quiet", str(source), str(root)],
                    source, environment, deadline)
            base = checked(["git", "rev-parse", "origin/main"],
                           source, environment, deadline).decode().strip()
            checked(["git", "update-ref", "refs/remotes/origin/main", base],
                    root, environment, deadline)
            # Permit pre-commit verification without importing other dirty files.
            for name in ("Makefile", ".gitleaks.toml", "scripts/lint_secrets.py"):
                (root / name).write_bytes((source / name).read_bytes())
            exercise(root, environment, deadline)
        return 0
    except GateFailure as error:
        print(f"FAIL: {error}")
    except (OSError, subprocess.SubprocessError):
        print("FAIL: tool unavailable, process timeout or fixture cleanup failure; output suppressed")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
