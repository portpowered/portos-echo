"""T3 public make/helper process witnesses with controlled tools; no real ARM build."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from wsl_distro import DEFAULT_WSL_DISTRO


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
elif name == 'uname':
    print(os.environ.get('TEST_PLATFORM', 'Linux'))
elif name == 'cygpath':
    print('C:/controlled checkout')
elif name == 'wsl':
    if os.environ.get('TEST_PLATFORM') == 'Linux':
        sys.exit('unexpected recursive WSL dispatch in controlled guest')
    if os.environ.get('WSL_FAILURE'):
        sys.exit('controlled WSL unavailable')
    assert args[:3] == ['-d', os.environ['EXPECTED_DISTRO'], '--exec'], args
    if args[3] == 'wslpath':
        if os.environ.get('PATH_FAILURE'):
            sys.exit(26)
        print(os.getcwd().replace('\\', '/') if args[5] == 'C:/controlled checkout' else
              json.loads(os.environ.get('PATH_TRANSLATIONS', '{}')).get(args[5], args[5]))
    else:
        import subprocess
        assert args[3] == 'sh' and args[4] in ('-lc', '-c'), args
        child = dict(os.environ, TEST_PLATFORM='Linux', HOME=os.environ['GUEST_HOME'])
        guest_args = args[5:]
        guest_args[3] = json.loads(os.environ.get('PATH_TRANSLATIONS', '{}')).get(guest_args[3], guest_args[3])
        target_index = 3 if args[4] == '-c' else 4
        guest_args[target_index] = json.loads(os.environ.get('GUEST_PATHS', '{}')).get(guest_args[target_index], guest_args[target_index])
        # The controlled guest uses the same shell, without host login startup files.
        sys.exit(subprocess.run(['sh', '-c', *guest_args], env=child).returncode)
else:
    sys.exit('unexpected tool: ' + name)
'''


class BuildEchoTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='build echo ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        for path in ('Makefile', 'scripts/build-echo.sh', 'scripts/build-echo-wsl.sh',
                     'scripts/make-posix.py', 'scripts/wsl-distro.sh'):
            shutil.copyfile(ROOT / path, self.root / path)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.witness = self.root / 'fake.py'
        self.witness.write_text(FAKE)
        self.log = self.root / 'commands.jsonl'
        self.env = dict(os.environ)
        for name in ('ANDROID_NDK_HOME', 'ANDROID_NDK_LATEST_HOME', 'MAKEFLAGS',
                     'MFLAGS', 'MAKELEVEL', 'CARGO_FAILURE', 'NO_BINARY',
                     'HEADERS', 'INSPECT_FAILURE', 'COMPILER_FAILURE', 'TEST_PLATFORM',
                     'WSL_FAILURE', 'PATH_FAILURE', 'PATH_TRANSLATIONS', 'PARTIAL_BINARY',
                     'GUEST_PATHS', 'ECHO_WSL_DISTRO'):
            self.env.pop(name, None)
        self.env.update(EXPECTED_DISTRO=DEFAULT_WSL_DISTRO, COMMAND_LOG=str(self.log), WITNESS=self.witness.as_posix(),
                        GUEST_HOME=self.root.as_posix(),
                        CARGO_TARGET_DIR=(self.root / 'output with spaces').as_posix())
        self.env['PATH'] = str(self.bin) + os.pathsep + self.env['PATH']
        if os.name == 'nt':
            git = Path(shutil.which('git.exe')).resolve()
            self.env['PATH'] += os.pathsep + str(git.parent.parent / 'usr/bin')
        self.shell = shutil.which('sh', path=self.env['PATH'])
        for name in ('cargo', 'rustc', 'file', 'curl', 'wget', 'rustup',
                     'uname', 'wsl', 'cygpath'):
            self.tool(self.bin / name)
        self.ndk = self.make_ndk('ndk with spaces')

    def test_windows_override_selects_every_guest_operation(self):
        self.env.update(ECHO_WSL_DISTRO='pe-ci', EXPECTED_DISTRO='pe-ci')
        self.make_ndk('android/android-ndk-r27d')
        result = self.run_helper(['echo-contracts'], TEST_PLATFORM='MINGW64_NT')
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [c for c in self.commands() if c['name'] == 'wsl']
        self.assertTrue(calls)
        self.assertTrue(all(c['args'][:3] == ['-d', 'pe-ci', '--exec'] for c in calls))

    def test_linux_build_ignores_invalid_windows_selector(self):
        result, _ = self.run_make(ECHO_WSL_DISTRO='bad;name', ANDROID_NDK_HOME=str(self.ndk))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(c['name'] == 'wsl' for c in self.commands()))

    def commands(self):
        records = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return [record for record in records if record['name'] != 'uname']

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
        selected = self.env | env
        # Preserve the controlled tools ahead of the Windows POSIX bootstrap.
        args = ['make', '--no-print-directory', 'build-echo']
        if os.name == 'nt':
            args.append('PATH=' + selected['PATH'])
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

    def test_windows_default_uses_guest_ndk_and_public_make(self):
        self.make_ndk('android/android-ndk-r27d')
        self.optional('echo-bridge')
        result, commands = self.run_make(TEST_PLATFORM='MINGW64_NT')
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo, = [c for c in commands if c['name'] == 'cargo']
        self.assertTrue(cargo['env']['ANDROID_NDK_HOME'].endswith('/android/android-ndk-r27d'))
        self.assertEqual(cargo['env']['RUSTUP_AUTO_INSTALL'], '0')
        self.assertIn('echo-bridge', cargo['args'])
        self.assertIn('binary size 19 bytes', result.stdout)

    def test_windows_paths_and_literal_packages_are_data(self):
        packages = ['echo-bridge', 'space package', 'glob*', 'semi;colon', '$(touch sentinel)']
        result = self.run_helper(packages, TEST_PLATFORM='MSYS_NT',
                                 ANDROID_NDK_HOME=self.ndk.as_posix())
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo, = [c for c in self.commands() if c['name'] == 'cargo']
        self.assertEqual(cargo['args'][7::2], packages)
        self.assertFalse((self.root / 'sentinel').exists())
        dispatch, = [c for c in self.commands() if c['name'] == 'wsl' and '-lc' in c['args']]
        self.assertEqual(dispatch['args'][-len(packages):], packages)

    def test_windows_explicit_invalid_and_tool_failures_remove_stale(self):
        self.env['CARGO_TARGET_DIR'] = 'output with spaces'
        self.make_ndk('android/android-ndk-r27d')
        for fault, diagnostic in [
                ({'ANDROID_NDK_HOME': (self.root / 'missing').as_posix()}, 'does not exist'),
                ({'WSL_FAILURE': '1'}, "WSL distro '" + self.env['EXPECTED_DISTRO'] + "'"),
                ({'PATH_FAILURE': '1'}, 'could not translate'),
                ({'COMPILER_FAILURE': '1'}, 'compiler failed'),
                ({'INSPECT_FAILURE': '1'}, 'inspection failed')]:
            with self.subTest(fault=fault):
                binary = self.stale_binary()
                result, _ = self.run_make(TEST_PLATFORM='CYGWIN_NT', **fault)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertFalse(binary.exists())
                self.assertNotIn('binary size', result.stdout)

    def test_windows_ndk_translation_failure_cleans_distinct_guest_output(self):
        guest_target = '/guest-only/output with spaces'
        binary = self.root / 'guest filesystem/armv7-linux-androideabi/release/echo-bridge'
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'stale guest artifact')
        result = self.run_helper(['echo-bridge'], TEST_PLATFORM='MINGW64_NT',
            CARGO_TARGET_DIR=guest_target, ANDROID_NDK_HOME='Z:/missing NDK',
            PATH_FAILURE='1', GUEST_PATHS=json.dumps({guest_target: binary.parents[2].as_posix()}))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('could not translate', result.stderr)
        self.assertFalse(binary.exists())
        self.assertNotIn('binary size', result.stdout)
        self.assertNotIn('cargo', [c['name'] for c in self.commands()])

    def test_windows_drive_unc_and_guest_paths_translate_without_evaluation(self):
        for selected in ('Z:/NDK with spaces', '//host/share/NDK with spaces',
                         '/guest/NDK with spaces'):
            with self.subTest(selected=selected):
                self.log.unlink(missing_ok=True)
                result = self.run_helper(['echo-config'], TEST_PLATFORM='MINGW64_NT',
                    ANDROID_NDK_HOME=selected,
                    PATH_TRANSLATIONS=json.dumps({selected: self.ndk.as_posix()}))
                self.assertEqual(result.returncode, 0, result.stderr)
                cargo, = [c for c in self.commands() if c['name'] == 'cargo']
                self.assertTrue(cargo['env']['ANDROID_NDK_HOME'].endswith('/ndk with spaces'))

    def test_windows_fallback_precedence_and_absent_default(self):
        result, _ = self.run_make(TEST_PLATFORM='MINGW64_NT')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('selected Android NDK directory does not exist', result.stderr)
        for primary, fallback, accepted in (
                ('', self.ndk.as_posix(), True),
                ((self.root / 'missing').as_posix(), self.ndk.as_posix(), False),
                ('', (self.root / 'missing').as_posix(), False)):
            with self.subTest(primary=primary, fallback=fallback):
                self.log.unlink(missing_ok=True)
                result, commands = self.run_make(TEST_PLATFORM='MINGW64_NT',
                    ANDROID_NDK_HOME=primary, ANDROID_NDK_LATEST_HOME=fallback)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)
                self.assertEqual(sum(c['name'] == 'cargo' for c in commands), int(accepted))

    def test_missing_wsl_reports_prerequisite_and_cleans_reachable_output(self):
        self.env['CARGO_TARGET_DIR'] = 'output with spaces'
        (self.bin / 'wsl').unlink()
        path = os.pathsep.join(entry for entry in self.env['PATH'].split(os.pathsep)
                               if not any((Path(entry) / name).is_file() for name in ('wsl', 'wsl.exe')))
        binary = self.stale_binary()
        result = self.run_helper(['echo-bridge'], TEST_PLATFORM='MINGW64_NT', PATH=path)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("installed WSL distro '" + self.env['EXPECTED_DISTRO'] + "' required", result.stderr)
        self.assertFalse(binary.exists())

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
                                   'echo-screen-server', 'echo-delegation', 'echo-signin'])
        # Windows environment mappings normalize variable names to uppercase.
        child = {key.upper(): value for key, value in cargo['env'].items()}
        self.assertTrue(child['ANDROID_NDK_HOME'].endswith('/ndk with spaces'))
        for key, tool in (
            ('CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER', 'armv7a-linux-androideabi24-clang'),
            ('CC_armv7_linux_androideabi', 'armv7a-linux-androideabi24-clang'),
            ('CXX_armv7_linux_androideabi', 'armv7a-linux-androideabi24-clang++'),
            ('AR_armv7_linux_androideabi', 'llvm-ar'),
        ):
            self.assertEqual(child[key.upper()], child['ANDROID_NDK_HOME'] +
                             '/toolchains/llvm/prebuilt/linux-x86_64/bin/' + tool)
        self.assertEqual(child['ANDROID_PLATFORM'], 'android-24')
        for package in packages:
            self.assertIn(f'build-echo: built {package} for armv7-linux-androideabi (release)', result.stdout)
        self.assertIn('library-only build', result.stdout)
        self.assertNotIn('binary size', result.stdout)
        self.assertEqual([c['name'] for c in commands],
                         ['armv7a-linux-androideabi24-clang', 'rustc', 'cargo'])

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
        binary.parent.mkdir(parents=True)
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


from test_wsl_distro import SelectorTests, DispatchTests, PythonDispatchTests


if __name__ == '__main__':
    unittest.main()
