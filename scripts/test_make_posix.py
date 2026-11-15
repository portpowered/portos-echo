"""T1 discovery and T0 actual-Make entry/format regressions; no product builds."""

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("make_posix", ROOT / "scripts/make-posix.py")
BOOT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BOOT)
WINDOWS = os.name == "nt"
MAKE = shutil.which("make")
GIT = shutil.which("git")


def snapshot():
    values = [os.environ.get("PATH")]
    if WINDOWS:
        import winreg

        for hive, key in (
            (winreg.HKEY_CURRENT_USER, "Environment"),
            (winreg.HKEY_LOCAL_MACHINE,
             r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
        ):
            try:
                with winreg.OpenKey(hive, key) as handle:
                    values.append(winreg.QueryValueEx(handle, "Path"))
            except FileNotFoundError:
                values.append(None)
    return values


class DiscoveryTests(unittest.TestCase):
    def test_supported_layouts_and_complete_tool_set(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "Git space é"
            tools = root / "usr/bin"
            tools.mkdir(parents=True)
            for name in BOOT.REQUIRED:
                (tools / (name + ".exe")).touch()
            for layout in ("cmd", "bin", "usr/bin", "mingw64/bin"):
                with self.subTest(layout=layout):
                    git = root / layout / "git.exe"
                    git.parent.mkdir(parents=True, exist_ok=True)
                    git.touch()
                    lookup = lambda command, path: str(git)
                    self.assertEqual(BOOT.discover("selected-only", lookup), tools.as_posix())

    def test_missing_git_and_unsupported_layout(self):
        with self.assertRaisesRegex(ValueError, "git.exe.*inherited PATH.*manually"):
            BOOT.discover("empty", lambda command, path: None)
        with self.assertRaisesRegex(ValueError, "Unsupported git.exe layout:.*manually"):
            BOOT.discover("shim", lambda command, path: str(Path("shim/git.exe").absolute()))

    def test_first_selected_git_is_not_replaced_by_another_installation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("first", "second"):
                (root / name / "cmd").mkdir(parents=True)
                (root / name / "cmd/git.exe").touch()
                (root / name / "usr/bin").mkdir(parents=True)
            for name in BOOT.REQUIRED:
                (root / "second/usr/bin" / (name + ".exe")).touch()
            lookup = lambda command, path: str(root / "first/cmd/git.exe")
            with self.assertRaisesRegex(ValueError, "Missing sh.exe, xargs.exe.*first/usr/bin"):
                BOOT.discover("first;second", lookup)
            (root / "first/usr/bin/sh.exe").touch()
            with self.assertRaisesRegex(ValueError, "Missing xargs.exe.*repair"):
                BOOT.discover("first;second", lookup)

    def test_command_output_and_exit_mapping(self):
        for args, error in ((["unexpected"], "Expected no arguments"), ([], "missing tool")):
            out, err = io.StringIO(), io.StringIO()
            with patch.object(os, "name", "nt"), \
                    patch.object(BOOT, "discover", side_effect=ValueError("missing tool")), \
                    redirect_stdout(out), redirect_stderr(err):
                self.assertEqual(BOOT.main(args), 1)
            self.assertEqual(out.getvalue(), "")
            self.assertIn(error, err.getvalue())
        out, err = io.StringIO(), io.StringIO()
        with patch.object(os, "name", "nt"), \
                patch.object(BOOT, "discover", return_value="C:/Git space/usr/bin"), \
                redirect_stdout(out), redirect_stderr(err):
            self.assertEqual(BOOT.main([]), 0)
        self.assertEqual(out.getvalue(), "C:/Git space/usr/bin\n")
        self.assertEqual(err.getvalue(), "")


class MakeTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(MAKE, "preinstalled GNU Make is required")
        self.assertIsNotNone(GIT, "preinstalled Git is required")
        self.before = snapshot()
        self.environment_before = dict(os.environ)
        self.temp = tempfile.TemporaryDirectory(prefix="make space é ")
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(lambda: self.assertEqual(self.before, snapshot()))
        self.addCleanup(lambda: self.assertEqual(self.environment_before, dict(os.environ)))
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/make-posix.py", self.root / "scripts")
        shutil.copy2(ROOT / "Makefile", self.root / "Makefile")
        with (self.root / "Makefile").open("a", encoding="utf-8", newline="\n") as file:
            file.write("\nprobe:\n\t@printf 'a\\0b\\0' | xargs -0 printf '<%s>\\n'\n"
                       "\t@sh -c 'command -v xargs >/dev/null && printf \"nested-ok\\n\"'\n"
                       "\t@printf 'complete\\n'\n"
                       "fail:\n\t@exit 23\n\t@printf 'must-not-run\\n'\n")

    def run_make(self, target, env=None):
        child = dict(os.environ if env is None else env)
        # Fixture output and failures must not depend on the invoking Make's flags.
        child.pop("MAKEFLAGS", None)
        child.pop("MFLAGS", None)
        return subprocess.run([MAKE, "--no-print-directory", target], cwd=self.root,
                              env=child, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              encoding="utf-8", errors="replace", timeout=120)

    def test_helper_copies_selected_environment(self):
        supplied = {"PATH": "fixture-only", "MAKEFLAGS": "--trace", "MFLAGS": "-s"}
        for env in (None, supplied):
            with self.subTest(explicit=env is not None):
                before = dict(os.environ if env is None else env)
                expected = before.copy()
                expected.pop("MAKEFLAGS", None)
                expected.pop("MFLAGS", None)
                with patch.object(subprocess, "run") as run:
                    self.run_make("probe", env)
                child = run.call_args.kwargs["env"]
                self.assertEqual(child, expected)
                self.assertIsNot(child, os.environ if env is None else env)
                self.assertEqual(dict(os.environ if env is None else env), before)

    def test_real_shell_nested_tools_and_recipe_failure(self):
        env = os.environ.copy()
        if WINDOWS:
            tools = os.path.normcase(os.path.normpath(BOOT.discover(env["PATH"])))
            env["PATH"] = os.pathsep.join(
                part for part in env["PATH"].split(os.pathsep)
                if os.path.normcase(os.path.normpath(part)) != tools)
            self.assertIsNone(shutil.which("xargs", path=env["PATH"]))
            self.assertIsNone(shutil.which("sh", path=env["PATH"]))
        absent = env.copy()
        absent.pop("MAKEFLAGS", None)
        absent.pop("MFLAGS", None)
        traced = env.copy()
        traced.update(MAKEFLAGS="--trace", MFLAGS="--trace")
        for name, selected in (("implicit", None), ("absent", absent), ("trace", traced)):
            with self.subTest(environment=name):
                before = dict(os.environ if selected is None else selected)
                result = self.run_make("probe", selected)
                self.assertEqual(result.returncode, 0, result.stdout)
                self.assertEqual(result.stdout.splitlines(), ["<a>", "<b>", "nested-ok", "complete"])
                self.assertEqual(dict(os.environ if selected is None else selected), before)
                result = self.run_make("fail", selected)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("Error 23", result.stdout)
                self.assertNotIn("must-not-run", result.stdout)
                self.assertEqual(dict(os.environ if selected is None else selected), before)

    def test_real_format_seed_then_repair(self):
        subprocess.run([GIT, "init", "-q"], cwd=self.root, check=True, timeout=30)
        (self.root / "src").mkdir()
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "bootstrap-fixture"\nversion = "0.0.0"\nedition = "2021"\n',
            encoding="utf-8", newline="\n")
        (self.root / "src/lib.rs").write_text("pub fn value() -> u8 {\n    1\n}\n", encoding="utf-8", newline="\n")
        seed = self.root / "scripts/format-seed.sh"
        seed.write_text('#!/bin/sh\nif true;then printf "%s\\n" ok;fi\n', encoding="utf-8", newline="\n")
        subprocess.run([GIT, "add", "."], cwd=self.root, check=True, timeout=30)
        result = self.run_make("fmt-check")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("cargo fmt --all --check", result.stdout)
        self.assertIn("format-seed.sh", result.stdout)
        self.assertIn("+", result.stdout)
        seed.write_text('#!/bin/sh\nif true; then\n  printf "%s\\n" ok\nfi\n', encoding="utf-8", newline="\n")
        result = self.run_make("fmt-check")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("cargo fmt --all --check", result.stdout)
        self.assertIn("shfmt -d", result.stdout)

    def contract_gate(self, fail_package, expected_packages):
        tools = self.root / "tools"
        tools.mkdir()
        cargo = tools / "cargo"
        cargo.write_text(
            '#!/bin/sh\nprintf "%s\\n" "$*" >> cargo-invocations.txt\n'
            'printf "cargo-stub:%s\\n" "$5"\n'
            'if [ "$5" = "$FAIL_PACKAGE" ]; then exit 23; fi\n',
            encoding="utf-8", newline="\n")
        cargo.chmod(0o755)
        if WINDOWS:
            (tools / "cargo.cmd").write_text(
                '@"' + BOOT.discover(os.environ["PATH"]) + '/sh.exe" "%~dp0cargo" %*\n',
                encoding="utf-8")
        env = {key: value for key, value in os.environ.items() if key.upper() != "PATH"}
        inherited = [part for part in os.environ["PATH"].split(os.pathsep)
                     if not any((Path(part) / name).is_file() for name in ("cargo", "cargo.exe"))]
        env["PATH"] = os.pathsep.join([str(tools), *inherited])
        env["FAIL_PACKAGE"] = fail_package
        result = self.run_make("test-contract", env)
        self.assertTrue((self.root / "cargo-invocations.txt").is_file(), result.stdout)
        commands = {
            "echo-contracts": "test --offline --locked -p echo-contracts --lib "
                              "--test contracts_wyoming --test contracts_events -- --nocapture",
            "echo-screen-contract": "test --offline --locked -p echo-screen-contract "
                                    "--test screen_wire -- --nocapture",
        }
        self.assertEqual(
            (self.root / "cargo-invocations.txt").read_text(encoding="utf-8").splitlines(),
            [commands[package] for package in expected_packages])
        for package in expected_packages:
            self.assertIn("cargo-stub:" + package, result.stdout)
        self.assertNotIn("no contract tests yet", result.stdout)
        return result

    def test_contract_gate_runs_both_locked_offline_suites(self):
        result = self.contract_gate("", ["echo-contracts", "echo-screen-contract"])
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_contract_gate_stops_after_first_suite_failure(self):
        result = self.contract_gate("echo-contracts", ["echo-contracts"])
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Error 23", result.stdout)
        self.assertNotIn("cargo-stub:echo-screen-contract", result.stdout)

    def test_contract_gate_propagates_screen_suite_failure(self):
        result = self.contract_gate("echo-screen-contract", ["echo-contracts", "echo-screen-contract"])
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Error 23", result.stdout)

    @unittest.skipUnless(WINDOWS, "Windows-only process coverage unavailable on native Unix")
    def test_missing_git_or_selected_installation_tools_fail_before_recipe(self):
        env = os.environ.copy()
        base = os.pathsep.join((str(Path(sys.executable).parent),
                               str(Path(os.environ["SystemRoot"]) / "System32")))
        env["PATH"] = base
        cases = [(None, "git.exe"), ("empty", "sh.exe"), ("sh-only", "xargs.exe")]
        for layout, diagnostic in cases:
            with self.subTest(layout=layout):
                if layout:
                    selected = self.root / layout
                    (selected / "cmd").mkdir(parents=True)
                    (selected / "cmd/git.exe").touch()
                    (selected / "usr/bin").mkdir(parents=True)
                    if layout == "sh-only":
                        (selected / "usr/bin/sh.exe").touch()
                    env["PATH"] = os.pathsep.join((str(selected / "cmd"), str(Path(GIT).parent), base))
                result = self.run_make("probe", env)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(diagnostic, result.stdout)
                self.assertIn("manually", result.stdout)
                self.assertIn("inherited PATH" if layout is None else "usr/bin", result.stdout)
                self.assertNotIn("complete", result.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
