"""T1: sourced shell helper calls with explicit command fakes and private files.

No network or compilation. The two real builds remain separate release evidence.
"""

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/build-codex-armv7.sh"


class BuildHelperTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="codex helper ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.work = self.root / "work tree"
        self.output = self.root / "output bundle"
        self.repo = self.root / "repository"
        self.repo.mkdir()

    def call(self, code, **values):
        env = dict(os.environ, CODEX_ARMV7_SOURCE_ONLY="1",
                   CODEX_ARMV7_WORK_DIR=str(self.work),
                   CODEX_ARMV7_OUTPUT_DIR=str(self.output))
        env.update(values)
        setup = '. "$0"\nrepo="$TEST_REPO"\n'
        env["TEST_REPO"] = str(self.repo)
        return subprocess.run(["sh", "-c", setup + code, str(SCRIPT)],
                              env=env, text=True, capture_output=True, timeout=10)

    def assert_failed(self, result, diagnostic=None):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("size_bytes=", result.stdout)
        self.assertNotIn("sha256=", result.stdout)
        if diagnostic:
            self.assertIn(diagnostic, result.stderr)

    def test_paths_reject_relative_overlap_repository_and_nonempty(self):
        for work, output, diagnostic in [
            ("relative", str(self.output), "absolute"),
            (str(self.work), str(self.work / "child"), "parent must exist"),
            (str(self.repo / "child"), str(self.output), "outside"),
            (str(self.root), str(self.output), "contain the repository"),
            (str(self.work), str(self.work), "overlap"),
        ]:
            with self.subTest(work=work, output=output):
                self.assert_failed(self.call("codex_paths", CODEX_ARMV7_WORK_DIR=work,
                                             CODEX_ARMV7_OUTPUT_DIR=output), diagnostic)
        self.work.mkdir()
        sentinel = self.work / "keep"
        sentinel.write_text("unchanged")
        self.assert_failed(self.call("codex_paths"), "new or empty")
        self.assertEqual(sentinel.read_text(), "unchanged")

    def test_physical_symlink_containment(self):
        alias = self.root / "alias"
        alias.symlink_to(self.repo, target_is_directory=True)
        self.assert_failed(self.call("codex_paths", CODEX_ARMV7_WORK_DIR=str(alias / "new")),
                           "outside")

    def test_invalid_ndk_prevents_work(self):
        result = self.call('ANDROID_NDK_HOME=/missing; codex_prerequisites')
        self.assert_failed(result, "missing NDK tool")
        self.assertFalse(self.work.exists())

    def test_missing_command_prevents_work(self):
        self.assert_failed(self.call('command() { return 1; }; codex_prerequisites'),
                           "missing prerequisite: git")

    def test_environment_overrides_worker_count_and_isolates_target(self):
        result = self.call('codex_paths; tools=/ndk/bin; ndk=/ndk; codex_environment; '
                           'printf "%s\\n%s\\n%s\\n" "$CARGO_BUILD_JOBS" '
                           '"$CARGO_TARGET_DIR" "$CC_armv7_linux_androideabi"',
                           CARGO_BUILD_JOBS="64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(),
                         ["4", str(self.work / "target"),
                          "/ndk/bin/armv7a-linux-androideabi24-clang"])

    def test_wrong_revision_and_patch_failure(self):
        fake = '''
work="$CODEX_ARMV7_WORK_DIR"; mkdir "$work"; commit=expected
git() {
  case "$*" in
    *rev-parse*) printf '%s\\n' "$REVISION" ;;
    *apply*) return 7 ;;
  esac
}
codex_source
'''
        self.assert_failed(self.call(fake, REVISION="wrong"), "wrong source identity")
        self.work.rmdir()
        patches = self.repo / "third_party/codex/patches"
        patches.mkdir(parents=True)
        (patches / "0001.patch").write_text("fake patch")
        self.assert_failed(self.call(fake, REVISION="expected"))

    def test_ca_download_and_checksum_failure(self):
        self.assert_failed(self.call('stage="$CODEX_ARMV7_OUTPUT_DIR"; '
                                    'curl() { return 8; }; codex_ca'))
        self.output.mkdir()
        result = self.call('stage="$CODEX_ARMV7_OUTPUT_DIR"; '
                           'curl() { printf wrong > "$stage/cacert.pem"; }; codex_ca')
        self.assert_failed(result, "FAILED")

    def pipeline(self, override=""):
        # Controlled component seams: real directory claiming/staging/publication.
        return self.call('''
codex_prerequisites() { compiler='rustc controlled'; tools=/ndk/bin; ndk=/ndk; }
codex_source() {
  mkdir -p "$work/source"
  printf license > "$work/source/LICENSE"
  printf notice > "$work/source/NOTICE"
}
codex_ca() { printf ca > "$stage/cacert.pem"; }
codex_build() { printf binary > "$stage/codex-app-server"; }
codex_inspect() {
  needed=libc.so; size=6
  digest=$(sha256sum "$stage/codex-app-server"); digest=${digest%% *}
}
''' + override + '\ncodex_main')

    def test_success_publishes_exact_bundle_and_records(self):
        result = self.pipeline()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(p.name for p in self.output.iterdir()),
                         ["LICENSE", "NOTICE", "cacert.pem", "codex-app-server"])
        self.assertEqual((self.output / "codex-app-server").read_bytes(), b"binary")
        records = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(records["size_bytes"], "6")
        self.assertEqual(records["sha256"], hashlib.sha256(b"binary").hexdigest())
        self.assertEqual(records["binary"], str(self.output / "codex-app-server"))
        self.assertEqual(records["needed"], "libc.so")
        self.assertEqual(list(self.root.glob("*.lock")), [])

    def test_every_stage_failure_and_signal_prevent_publication(self):
        for stage in ["codex_source", "codex_ca", "codex_build", "codex_inspect"]:
            with self.subTest(stage=stage):
                result = self.pipeline(stage + '() { return 9; }')
                self.assert_failed(result)
                self.assertFalse(self.output.exists())
                self.assertEqual(list(self.root.glob("*.lock")), [])
                self.assertEqual(list(self.root.glob("*.staging.*")), [])
                # Failed builds remain available for diagnosis; use a new work dir.
                self.work = self.root / (stage + " next")
        self.assert_failed(self.pipeline('codex_build() { kill -TERM $$; }'))
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.root.glob("*.staging.*")), [])

    def test_prior_output_is_preserved_and_claims_exclude_other_invocations(self):
        self.output.mkdir()
        sentinel = self.output / "codex-app-server"
        sentinel.write_text("prior")
        self.assert_failed(self.pipeline(), "new or empty")
        self.assertEqual(sentinel.read_text(), "prior")
        sentinel.unlink()
        self.output.rmdir()
        lock = Path(str(self.work) + ".lock")
        lock.mkdir()
        self.assert_failed(self.pipeline(), "work already claimed")
        self.assertTrue(lock.is_dir())

    def test_staging_copy_or_publication_failure_leaves_no_bundle(self):
        for command in ["cp", "mv"]:
            with self.subTest(command=command):
                self.assert_failed(self.pipeline(command + '() { return 28; }'))
                self.assertFalse(self.output.exists())
                self.assertEqual(list(self.root.glob("*.staging.*")), [])
                self.assertEqual(list(self.root.glob("*.lock")), [])
                self.work = self.root / (command + " next")

    def test_independent_invocations_publish_separately(self):
        self.assertEqual(self.pipeline().returncode, 0)
        first = self.output
        self.work = self.root / "second work"
        self.output = self.root / "second output"
        self.assertEqual(self.pipeline().returncode, 0)
        self.assertEqual((first / "codex-app-server").read_bytes(), b"binary")
        self.assertEqual((self.output / "codex-app-server").read_bytes(), b"binary")

    def test_build_and_strip_failures_propagate(self):
        self.work.mkdir()
        (self.work / "source/codex-rs").mkdir(parents=True)
        tools = self.root / "tools"
        tools.mkdir()
        strip = tools / "llvm-strip"
        strip.write_text("#!/bin/sh\nexit 7\n")
        strip.chmod(0o755)
        self.output.mkdir()
        common = ('work="$CODEX_ARMV7_WORK_DIR"; stage="$CODEX_ARMV7_OUTPUT_DIR"; '
                  'tools="$TEST_TOOLS"; ')
        self.assert_failed(self.call(common + 'cargo() { return 6; }; codex_build',
                                     TEST_TOOLS=str(tools)))
        artifact = self.work / "target/armv7-linux-androideabi/release/codex-app-server"
        artifact.parent.mkdir(parents=True)
        artifact.write_bytes(b"binary")
        self.assert_failed(self.call(common + 'cargo() { :; }; codex_build',
                                     TEST_TOOLS=str(tools)))

    def test_elf_properties_and_all_dependencies_are_checked(self):
        self.output.mkdir()
        (self.output / "codex-app-server").write_bytes(b"binary")
        tools = self.root / "tools"
        tools.mkdir()
        readelf = tools / "llvm-readelf"
        readelf.write_text('#!/bin/sh\nprintf "%s\\n" "$TEST_HEADERS"\n')
        readelf.chmod(0o755)
        good = ('Class: ELF32\nMachine: ARM\nFlags: 0x5000200\n'
                '[Requesting program interpreter: /system/bin/linker]\n'
                '(NEEDED) Shared library: [libc.so]\n'
                '(NEEDED) Shared library: [libdl.so]')
        code = ('stage="$CODEX_ARMV7_OUTPUT_DIR"; tools="$TEST_TOOLS"; '
                'file() { printf "%s\\n" "$TEST_FILE"; }; codex_inspect; '
                'printf "%s\\n%s\\n%s\\n" "$needed" "$size" "$digest"')
        description = 'ELF 32-bit LSB pie executable, ARM, EABI5 interpreter /system/bin/linker'
        result = self.call(code, TEST_TOOLS=str(tools), TEST_HEADERS=good,
                           TEST_FILE=description)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(),
                         ["libc.so,libdl.so", "6", hashlib.sha256(b"binary").hexdigest()])
        bad_headers = [good.replace("ELF32", "ELF64"), good.replace("ARM", "X86"),
                       good.replace("0x5000200", "0x4000200"),
                       good.replace("/system/bin/linker", "/lib/ld-linux.so"),
                       good.replace("libdl.so", "libssl.so"),
                       good.replace("libdl.so", "libstdc++.so.6"),
                       good.split("(NEEDED)")[0]]
        for headers in bad_headers:
            with self.subTest(headers=headers):
                self.assert_failed(self.call(code, TEST_TOOLS=str(tools),
                                             TEST_HEADERS=headers, TEST_FILE=description))
        self.assert_failed(self.call(code, TEST_TOOLS=str(tools), TEST_HEADERS=good,
                                     TEST_FILE="PE32 Windows executable"))
        readelf.write_text("#!/bin/sh\nexit 8\n")
        self.assert_failed(self.call(code, TEST_TOOLS=str(tools), TEST_HEADERS=good,
                                     TEST_FILE=description))


if __name__ == "__main__":
    unittest.main()
