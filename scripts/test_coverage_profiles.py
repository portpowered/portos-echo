"""T1 admission policy; --instrumented adds T3 real LLVM merge regressions.

The invocation driver builds one reusable local Rust fixture before the T3
suite. Tests consume that artifact in isolated directories, without networking.
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import coverage_profiles as profiles


ROOT = Path(__file__).resolve().parent.parent
ARTIFACT = None
TOOL = None


class PolicyTests(unittest.TestCase):
    def test_module_signature_separates_pool_and_rejects_unknown_names(self):
        for name in ("portos-echo-5519-8660843643471611737_0.profraw",
                     "portos-echo-5447-8660843643471611737.profraw"):
            self.assertEqual(profiles.module_signature(name), "8660843643471611737")
        for name in ("writer.profraw", "portos-echo-1-invalid_0.profraw",
                     "portos-echo-1-123_0.profraw\n"):
            self.assertEqual(profiles.module_signature(name), "unknown")

    def test_no_inputs_refuses_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(subprocess, "run"):
                with self.assertRaisesRegex(ValueError, "remaining=0"):
                    profiles.validate(root, root / "quarantine", "tool")

    def test_missing_tool_preserves_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            raw = root / "writer.profraw"
            raw.write_bytes(b"preserved")
            with patch.object(subprocess, "run", side_effect=FileNotFoundError):
                with self.assertRaises(FileNotFoundError):
                    profiles.validate(root, root / "quarantine", "absent")
            self.assertEqual(raw.read_bytes(), b"preserved")
            self.assertFalse((root / "quarantine").exists())

    def test_annotation_is_one_escaped_line(self):
        self.assertEqual(profiles.annotation("name%\r\n::error::"),
                         "name%25%0D%0A::error::")


class InstrumentedTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="coverage-regression-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.raw = self.root / "good.profraw"
        subprocess.run([str(ARTIFACT)], check=True,
                       env={**os.environ, "LLVM_PROFILE_FILE": str(self.raw)})
        self.quarantine = self.root / "quarantine"

    def merge(self, output):
        return subprocess.run(
            [TOOL, "merge", "-sparse", *map(str, sorted(self.root.glob("*.profraw"))),
             "-o", str(output)], capture_output=True,
        )

    def admit(self):
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/coverage_profiles.py"),
             "--profile-dir", str(self.root), "--quarantine", str(self.quarantine),
             "--profdata", TOOL], capture_output=True, text=True,
        )

    def test_one_truncated_warns_then_gates_run(self):
        bad = self.root / "portos-echo-5519-8660843643471611737_0.profraw"
        truncated = self.raw.read_bytes()[:16]
        bad.write_bytes(truncated)
        old = self.merge(self.root / "old.profdata")
        self.assertNotEqual(old.returncode, 0)
        self.assertIn(b"file header is corrupt", old.stderr)
        new = self.admit()
        self.assertEqual(new.returncode, 0, new.stderr)
        self.assertEqual(new.stdout.count("::warning::"), 1)
        self.assertIn(bad.name, new.stdout)
        self.assertIn("module_signature=8660843643471611737", new.stdout)
        self.assertIn("file header is corrupt", new.stdout)
        self.assertFalse(bad.exists())
        retained = list(self.quarantine.glob("run-*/*.profraw"))
        self.assertEqual([path.read_bytes() for path in retained], [truncated])
        merged = self.merge(self.root / "new.profdata")
        self.assertEqual(merged.returncode, 0, merged.stderr)
        print("one bad: old exit=1 (file header is corrupt); new exit=0; warnings=1; merge gate=0")

    def test_two_bad_fail_systemic(self):
        truncated = self.raw.read_bytes()[:16]
        for name in ("bad1", "bad2"):
            (self.root / f"{name}.profraw").write_bytes(truncated)
        result = self.admit()
        self.assertEqual(result.returncode, 1)
        self.assertIn("Systemic coverage profile problem", result.stderr)
        self.assertIn("quarantined=2, remaining=1", result.stderr)
        self.assertEqual(result.stdout.count("::warning::"), 2)
        self.assertEqual(len(list(self.quarantine.glob("run-*/*.profraw"))), 2)
        self.assertEqual(result.stdout.count("module_signature=unknown"), 2)
        self.assertEqual([p.read_bytes() for p in self.quarantine.glob("run-*/*.profraw")],
                         [truncated, truncated])
        print("two bad: exit=1; warnings=2; systemic refusal; remaining=1")

    def test_only_bad_fails_systemic(self):
        self.raw.write_bytes(self.raw.read_bytes()[:16])
        truncated = self.raw.read_bytes()
        result = self.admit()
        self.assertEqual(result.returncode, 1)
        self.assertIn("quarantined=1, remaining=0", result.stderr)
        self.assertEqual([p.read_bytes() for p in self.quarantine.glob("run-*/*.profraw")],
                         [truncated])

    def test_all_good_byte_identical(self):
        second = self.root / "second.profraw"
        subprocess.run([str(ARTIFACT)], check=True,
                       env={**os.environ, "LLVM_PROFILE_FILE": str(second)})
        before = {path.name: path.read_bytes() for path in (self.raw, second)}
        old = self.root / "old.profdata"
        self.assertEqual(self.merge(old).returncode, 0)
        result = self.admit()
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        new = self.root / "new.profdata"
        self.assertEqual(self.merge(new).returncode, 0)
        self.assertEqual(old.read_bytes(), new.read_bytes())
        cov = str(Path(TOOL).with_name("llvm-cov"))
        exports = [subprocess.check_output(
            [cov, "export", "--format=lcov", str(ARTIFACT), f"--instr-profile={path}"]
        ) for path in (old, new)]
        self.assertEqual(exports[0], exports[1])
        self.assertEqual(before, {path.name: path.read_bytes() for path in (self.raw, second)})
        self.assertFalse(self.quarantine.exists())
        print("all good: exit=0; warnings=0; raw inputs, merged output and LCOV byte-identical")

    def test_recipe_runs_unchanged_gates_only_after_admission(self):
        # Substitute workspace Cargo commands, retaining the real Make recipe,
        # validator, profiles, and LLVM merge. No workspace build or gate claim.
        raw_dir = self.root / "target/llvm-cov-target"
        raw_dir.mkdir(parents=True)
        shutil.copytree(ROOT / "scripts", self.root / "scripts",
                        ignore=shutil.ignore_patterns("__pycache__"))
        # The outer driver already runs the real instrumented suite. Substitute
        # its recursive invocation while observing the Make command boundary.
        (self.root / "scripts/test_coverage_profiles.py").write_text(
            'import json, pathlib, sys\n'
            'pathlib.Path("self-test.json").write_text(json.dumps(sys.argv[1:]))\n'
        )
        # Cargo is substituted below, so no prebuilt writer exists in this
        # fixture. Observe that boundary; the real writer has its own T3 suite.
        tests = self.root / "tests"
        tests.mkdir()
        (tests / "test_windows_profraw.py").write_text(
            'import json, pathlib, sys\n'
            'pathlib.Path("writer-test.json").write_text(json.dumps(sys.argv[1:]))\n'
        )
        shutil.copyfile(ROOT / "tests/coverage_children.py", tests / "coverage_children.py")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        (bin_dir / "python").symlink_to(sys.executable)
        cargo = bin_dir / "cargo"
        cargo.write_text(f"#!{sys.executable}\n" + '''import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
with open("calls.jsonl", "a") as log:
    log.write(json.dumps(args) + "\\n")
if "report" in args:
    inputs = sorted(pathlib.Path("target/llvm-cov-target").glob("*.profraw"))
    sys.exit(subprocess.call([os.environ["LLVM_PROFDATA"], "merge", "-sparse",
                             *map(str, inputs), "-o", "report.profdata"]))
if "coverage-check" in args:
    sys.exit(int(os.environ.get("CHECK_EXIT", "0")))
''')
        cargo.chmod(0o755)
        overrides = self.root / "override.mk"
        overrides.write_text("hassmic-host codex-app-server:\n\t@true\n")
        environment = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}",
                       "LLVM_PROFDATA": TOOL}
        # Every selection the coverage recipe runs, read from the Makefile itself, so a lane that adds a
        # selection (as the bridge did) does not have to edit this count.
        makefile = (ROOT / "Makefile").read_text()
        recipe = makefile.split("\ncoverage: hassmic-host\n", 1)[1].split("\n\n", 1)[0]
        expected_selections = sum(
            "llvm-cov" in line and "--no-report" in line for line in recipe.splitlines()
        )
        self.assertGreaterEqual(expected_selections, 1)
        for bad_count, gate_exit in ((1, 0), (2, 0), (0, 23)):
            with self.subTest(bad_count=bad_count, gate_exit=gate_exit):
                for path in raw_dir.glob("*.profraw"):
                    path.unlink()
                shutil.copyfile(self.raw, raw_dir / "good.profraw")
                for index in range(bad_count):
                    (raw_dir / f"bad{index}.profraw").write_bytes(self.raw.read_bytes()[:16])
                log = self.root / "calls.jsonl"
                log.unlink(missing_ok=True)
                self_test = self.root / "self-test.json"
                self_test.unlink(missing_ok=True)
                writer_test = self.root / "writer-test.json"
                writer_test.unlink(missing_ok=True)
                result = subprocess.run(
                    ["make", "-f", str(ROOT / "Makefile"), "-f", str(overrides), "coverage"],
                    cwd=self.root, env={**environment, "CHECK_EXIT": str(gate_exit)},
                    capture_output=True, text=True,
                )
                calls = [json.loads(line) for line in log.read_text().splitlines()]
                self.assertEqual(json.loads(self_test.read_text()), ["--instrumented"])
                self.assertEqual(json.loads(writer_test.read_text()),
                                 ["--writer", "target/llvm-cov-target/debug/echo-lab-fake-adb"])
                self.assertEqual(sum("--no-report" in call for call in calls), expected_selections)
                reports = [call for call in calls if "report" in call]
                checks = [call for call in calls if "coverage-check" in call]
                self.assertEqual((len(reports), len(checks)),
                                 (0, 0) if bad_count == 2 else (1, 1))
                self.assertEqual(result.returncode == 0, bad_count == 1 and gate_exit == 0,
                                 result.stdout + result.stderr)
                if reports:
                    index = reports[0].index("--fail-under-lines")
                    self.assertEqual(reports[0][index + 1], "80")
                    self.assertIn("--no-default-ignore-filename-regex", reports[0])
        print(f"Make recipe: selections={expected_selections}; "
              "one bad gates=1/1; two bad gates=0/0; gate failure retained")


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(PolicyTests)
    with tempfile.TemporaryDirectory(prefix="coverage-fixture-") as directory:
        if "--instrumented" in sys.argv:
            TOOL = profiles.profdata_tool()
            source = Path(directory) / "writer.rs"
            source.write_text("fn main() { std::hint::black_box(42); }\n")
            ARTIFACT = Path(directory) / "writer"
            subprocess.run(["rustc", f"+{profiles.TOOLCHAIN}", "-C", "instrument-coverage",
                            str(source), "-o", str(ARTIFACT)], check=True)
            suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(InstrumentedTests))
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        sys.exit(not result.wasSuccessful())
