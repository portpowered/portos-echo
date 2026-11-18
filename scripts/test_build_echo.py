"""T3 public make/helper process witnesses with controlled tools; no real ARM build."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
MISSING = ('build-echo: set ANDROID_NDK_HOME (or ANDROID_NDK_LATEST_HOME) '
           'to an installed Android NDK; no download attempted')
FAKE = r'''
import json, os, pathlib, sys
name, *args = sys.argv[1:]
with open(os.environ['COMMAND_LOG'], 'a') as log:
    log.write(json.dumps({'name': name, 'args': args, 'env': dict(os.environ)}) + '\n')
if name == 'cargo':
    if os.environ.get('CARGO_FAILURE'):
        if os.environ.get('PARTIAL_BINARY'):
            binary = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'armv7-linux-androideabi/release/echo-bridge'
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_bytes(b'partial')
        print(os.environ['CARGO_FAILURE'], file=sys.stderr)
        sys.exit(23)
    if 'echo-bridge' in args and not os.environ.get('NO_BINARY'):
        binary = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'armv7-linux-androideabi/release/echo-bridge'
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(b'controlled artifact')
elif name == 'llvm-readelf':
    if os.environ.get('INSPECT_FAILURE'):
        sys.exit(24)
    print(os.environ.get('HEADERS', ''' + '"""' + r'''
  Class: ELF32
  Machine: ARM
  Flags: 0x5000200, Version5 EABI
  [Requesting program interpreter: /system/bin/linker]
  (NEEDED) Shared library: [libc.so]
''' + '"""' + r'''))
elif name.startswith('armv7a-'):
    if os.environ.get('COMPILER_FAILURE'):
        sys.exit(25)
    print('controlled clang version')
elif name == 'rustc':
    print('controlled rustc version')
elif name == 'file':
    print('controlled file: ELF 32-bit ARM EABI5')
else:
    sys.exit('unexpected tool: ' + name)
'''


class BuildEchoTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='build echo ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        for path in ('Makefile', 'scripts/build-echo.sh'):
            shutil.copyfile(ROOT / path, self.root / path)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.witness = self.root / 'fake.py'
        self.witness.write_text(FAKE)
        self.log = self.root / 'commands.jsonl'
        self.env = dict(os.environ)
        for name in ('ANDROID_NDK_HOME', 'ANDROID_NDK_LATEST_HOME', 'MAKEFLAGS',
                     'MFLAGS', 'MAKELEVEL', 'CARGO_FAILURE', 'NO_BINARY',
                     'HEADERS', 'INSPECT_FAILURE', 'COMPILER_FAILURE', 'PARTIAL_BINARY'):
            self.env.pop(name, None)
        self.env.update(COMMAND_LOG=str(self.log), WITNESS=self.witness.as_posix(),
                        CARGO_TARGET_DIR=(self.root / 'output with spaces').as_posix())
        self.env['PATH'] = str(self.bin) + os.pathsep + self.env['PATH']
        self.shell = shutil.which('sh', path=self.env['PATH'])
        for name in ('cargo', 'rustc', 'file', 'curl', 'wget', 'rustup'):
            self.tool(self.bin / name)
        self.ndk = self.make_ndk('ndk with spaces')
        self.optional('echo-bridge')


    def test_paths_and_literal_packages_are_data(self):
        packages = ['echo-bridge', 'space package', 'glob*', 'semi;colon', '$(touch sentinel)']
        result = self.run_helper(packages, ANDROID_NDK_HOME=self.ndk.as_posix())
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo, = [c for c in self.commands() if c['name'] == 'cargo']
        self.assertEqual(cargo['args'][7::2], packages)
        self.assertEqual(cargo['env']['RUSTUP_AUTO_INSTALL'], '0')
        self.assertFalse((self.root / 'sentinel').exists())

    def test_library_only_helper_keeps_binary_unproved(self):
        result = self.run_helper(['echo-config'], ANDROID_NDK_HOME=self.ndk.as_posix())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('library-only build', result.stdout)
        self.assertNotIn('binary size', result.stdout)
        self.assertEqual([c['name'] for c in self.commands()],
                         ['armv7a-linux-androideabi24-clang', 'rustc', 'cargo'])

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def tool(self, path):
        path.write_text('#!/bin/sh\nexec python "$WITNESS" "$(basename "$0")" "$@"\n',
                        newline='\n')
        path.chmod(0o755)

    def make_ndk(self, name):
        ndk = self.root / name
        tools = ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin'
        tools.mkdir(parents=True)
        for tool in ('armv7a-linux-androideabi24-clang',
                     'armv7a-linux-androideabi24-clang++', 'llvm-ar', 'llvm-readelf'):
            self.tool(tools / tool)
        cmake = ndk / 'build/cmake/android.toolchain.cmake'
        cmake.parent.mkdir(parents=True)
        cmake.touch()
        (ndk / 'source.properties').write_text('Pkg.Revision = controlled\n')
        return ndk

    def run_make(self, **env):
        args = ['make', '--no-print-directory', 'build-echo']
        result = subprocess.run(args,
                                cwd=self.root, env=self.env | env,
                                capture_output=True, text=True, timeout=30)
        return result, self.commands()

    def stale_binary(self):
        self.optional('echo-bridge')
        binary = Path(self.env['CARGO_TARGET_DIR']) / 'armv7-linux-androideabi/release/echo-bridge'
        if not binary.is_absolute():
            binary = self.root / binary
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(b'stale')
        return binary

    def run_helper(self, packages, **env):
        return subprocess.run([self.shell, 'scripts/build-echo.sh', *packages],
                              cwd=self.root, env=self.env | env,
                              capture_output=True, text=True, timeout=30)


    def test_cargo_partial_failure_removes_unpublishable_binary(self):
        binary = self.stale_binary()
        result, commands = self.run_make(ANDROID_NDK_HOME=self.ndk.as_posix(),
                                         CARGO_FAILURE='link failed', PARTIAL_BINARY='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('link failed', result.stderr)
        self.assertFalse(binary.exists())
        self.assertNotIn('llvm-readelf', [c['name'] for c in commands])
        self.assertNotIn('build-echo: built', result.stdout)

    def test_native_early_failures_remove_stale_and_empty_selection_fails(self):
        binary = self.stale_binary()
        result, _ = self.run_make()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(binary.exists())
        result = self.run_helper([], ANDROID_NDK_HOME=self.ndk.as_posix())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no packages selected', result.stderr)

    def optional(self, name):
        manifest = self.root / 'crates' / name / 'Cargo.toml'
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.touch()

    def test_missing_ndk_fails_before_any_tools(self):
        result, commands = self.run_make()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stderr.count(MISSING), 1)
        self.assertEqual(commands, [])

    def test_invalid_primary_does_not_use_valid_fallback(self):
        result, commands = self.run_make(ANDROID_NDK_HOME=str(self.root / 'missing'),
                                         ANDROID_NDK_LATEST_HOME=str(self.ndk))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('selected Android NDK directory does not exist', result.stderr)
        self.assertEqual(commands, [])

    def test_incomplete_ndk_fails_before_cargo(self):
        (self.ndk / 'toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar').unlink()
        result, commands = self.run_make(ANDROID_NDK_HOME=str(self.ndk))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(commands, [])

    def test_primary_precedence_target_environment_and_packages(self):
        fallback = self.make_ndk('fallback')
        result, commands = self.run_make(ANDROID_NDK_HOME=str(self.ndk),
                                         ANDROID_NDK_LATEST_HOME=str(fallback))
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo, = [c for c in commands if c['name'] == 'cargo']
        self.assertEqual(cargo['args'][:6], ['build', '--offline', '--locked', '--release',
                                           '--target', 'armv7-linux-androideabi'])
        packages = cargo['args'][7::2]
        self.assertEqual(packages, ['echo-contracts', 'echo-config', 'echo-wyoming',
                                   'echo-audio', 'echo-live', 'echo-chatgpt-auth',
                                   'echo-agent-api', 'echo-agent-loop', 'echo-mcp-auth',
                                   'echo-mcp-client', 'echo-screen-contract',
                                   'echo-screen-server', 'echo-delegation', 'echo-signin',
                                   'echo-bridge'])
        child = cargo['env']
        self.assertTrue(child['ANDROID_NDK_HOME'].endswith('/ndk with spaces'))
        for key, tool in (
            ('CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER', 'armv7a-linux-androideabi24-clang'),
            ('CC_armv7_linux_androideabi', 'armv7a-linux-androideabi24-clang'),
            ('CXX_armv7_linux_androideabi', 'armv7a-linux-androideabi24-clang++'),
            ('AR_armv7_linux_androideabi', 'llvm-ar'),
        ):
            self.assertEqual(child[key], child['ANDROID_NDK_HOME'] +
                             '/toolchains/llvm/prebuilt/linux-x86_64/bin/' + tool)
        self.assertEqual(child['ANDROID_PLATFORM'], 'android-24')
        for package in packages:
            self.assertIn(f'build-echo: built {package} for armv7-linux-androideabi (release)', result.stdout)
        self.assertIn('binary size 19 bytes', result.stdout)
        self.assertEqual([c['name'] for c in commands],
                         ['armv7a-linux-androideabi24-clang', 'rustc', 'cargo', 'file', 'llvm-readelf'])

    def test_fallback_and_empty_primary(self):
        for primary in (None, ''):
            with self.subTest(primary=primary):
                env = {'ANDROID_NDK_LATEST_HOME': str(self.ndk)}
                if primary is not None:
                    env['ANDROID_NDK_HOME'] = primary
                result, _ = self.run_make(**env)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_target_cache_and_build_errors_propagate_without_success(self):
        for error in ('target std missing', 'offline cache missing', 'opusic-sys compile error'):
            with self.subTest(error=error):
                result, _ = self.run_make(ANDROID_NDK_HOME=str(self.ndk), CARGO_FAILURE=error)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stderr)
                self.assertNotIn('build-echo: built', result.stdout)

    def test_compiler_error_prevents_cargo(self):
        result, commands = self.run_make(ANDROID_NDK_HOME=str(self.ndk), COMPILER_FAILURE='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([c['name'] for c in commands], ['armv7a-linux-androideabi24-clang'])

    def test_optional_packages_and_inspection(self):
        self.optional('echo-bridge')
        self.optional('echo-link')
        result, commands = self.run_make(ANDROID_NDK_HOME=str(self.ndk))
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo, = [c for c in commands if c['name'] == 'cargo']
        self.assertEqual(cargo['args'][-4:], ['-p', 'echo-bridge', '-p', 'echo-link'])
        inspector, = [c for c in commands if c['name'] == 'llvm-readelf']
        self.assertEqual(inspector['args'][:3], ['-h', '-l', '-d'])
        self.assertIn('binary size 19 bytes', result.stdout)

    def test_missing_binary_does_not_inspect_stale_artifact(self):
        self.optional('echo-bridge')
        binary = Path(self.env['CARGO_TARGET_DIR']) / 'armv7-linux-androideabi/release/echo-bridge'
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(b'stale')
        result, commands = self.run_make(ANDROID_NDK_HOME=str(self.ndk), NO_BINARY='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(binary.exists())
        self.assertNotIn('llvm-readelf', [c['name'] for c in commands])
        self.assertNotIn('binary size', result.stdout)

    def test_numeric_eabi_flags_control_artifact_acceptance(self):
        self.optional('echo-bridge')
        for flags, accepted in (('0x5000200', True),
                                ('0x05000200, Version5 EABI', True),
                                ('0x4000200', False),
                                ('0x6000200', False),
                                ('0x0', False),
                                ('0x15000200', False),
                                ('0x50002000', False),
                                ('0x4000200, Version5 EABI', False)):
            with self.subTest(flags=flags):
                headers = ('Class: ELF32\nMachine: ARM\n'
                           f'  Flags:                             {flags}\n'
                           '[Requesting program interpreter: /system/bin/linker]\n')
                result, _ = self.run_make(ANDROID_NDK_HOME=str(self.ndk), HEADERS=headers)
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn('binary size 19 bytes', result.stdout)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse((Path(self.env['CARGO_TARGET_DIR']) / 'armv7-linux-androideabi/release/echo-bridge').exists())
                    self.assertIn('expected Version5 EABI', result.stderr)
                    self.assertNotIn('binary size', result.stdout)

    def test_inspector_errors_and_wrong_artifact_properties_fail(self):
        self.optional('echo-bridge')
        valid = ('Class: ELF32\nMachine: ARM\nFlags: 0x5000200, Version5 EABI\n'
                 '[Requesting program interpreter: /system/bin/linker]\n')
        cases = [({'INSPECT_FAILURE': '1'}, ''),
                 ({'HEADERS': valid.replace('ELF32', 'ELF64')}, 'expected ELF32'),
                 ({'HEADERS': valid.replace('Machine: ARM', 'Machine: AArch64')}, 'expected ARM'),
                 ({'HEADERS': valid.replace('0x5000200, Version5', '0x4000200, Version4')}, 'expected Version5'),
                 ({'HEADERS': valid.replace('/system/bin/linker', '/lib/ld-linux.so')}, 'expected Android'),
                 ({'HEADERS': valid + '(NEEDED) [libssl.so]\n'}, 'unexpected OpenSSL'),
                 ({'HEADERS': valid + '(NEEDED) [libcrypto.so]\n'}, 'unexpected OpenSSL')]
        for env, diagnostic in cases:
            with self.subTest(env=env):
                result, _ = self.run_make(ANDROID_NDK_HOME=str(self.ndk), **env)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((Path(self.env['CARGO_TARGET_DIR']) / 'armv7-linux-androideabi/release/echo-bridge').exists())
                self.assertIn(diagnostic, result.stderr)
                self.assertNotIn('binary size', result.stdout)


if __name__ == '__main__':
    unittest.main()
