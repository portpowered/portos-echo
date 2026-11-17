"""Selector units and T3 controlled dispatch: never forward to a real WSL."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from wsl_distro import DEFAULT_WSL_DISTRO, resolve_wsl_distro

ROOT = Path(__file__).resolve().parents[1]
ADAPTERS = ('scripts/hassmic-host.sh', 'scripts/build-echo-wsl.sh',
            'tests/wyoming_host.sh', 'tests/bridge_host.sh',
            'tests/hassmic_host_smoke.sh', 'crates/echo-bridge/tests/bridge_refusal/host.sh')
INVALID = ('bad;name', 'a b', 'a/b', "a'b", 'a"b', 'a\nb', 'é', 'a\\b', 'a\tb')
TOOL = r'''
import json, os, sys, pathlib
name, *args = sys.argv[1:]
if name == 'uname':
    print(os.environ.get('PLATFORM', 'MINGW64_NT'))
elif name == 'cygpath':
    print('C:/controlled checkout')
else:
    with open(os.environ['AUDIT'], 'a') as audit:
        audit.write(json.dumps([name, args]) + '\n')
    if name == 'wsl':
        assert args[:3] == ['-d', os.environ['EXPECTED_DISTRO'], '--exec'], args
        if os.environ.get('DENY'):
            sys.exit(29)
        if args[3] == 'wslpath':
            print(args[-1])
        else:
            sys.exit(int(os.environ.get('GUEST_STATUS', '0')))
'''


def shell_env():
    env = dict(os.environ)
    env.pop('ECHO_WSL_DISTRO', None)
    if os.name == 'nt':
        git = Path(shutil.which('git.exe')).resolve()
        env['PATH'] += os.pathsep + str(git.parent.parent / 'usr/bin')
    return env


class SelectorTests(unittest.TestCase):
    def test_both_resolvers_have_exact_default_override_and_rejection(self):
        env = shell_env()
        for value in (None, '', 'pe-ci', 'A.z_9-0', *INVALID):
            with self.subTest(value=value):
                env.pop('ECHO_WSL_DISTRO', None)
                if value is not None:
                    env['ECHO_WSL_DISTRO'] = value
                result = subprocess.run(['sh', '-c', '. "$1"; echo_wsl_distro', 'sh',
                                         str(ROOT / 'scripts/wsl-distro.sh')],
                                        env=env, text=True, capture_output=True)
                if value in INVALID:
                    with self.assertRaisesRegex(ValueError, 'ECHO_WSL_DISTRO'):
                        resolve_wsl_distro(env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, '')
                    self.assertIn('ECHO_WSL_DISTRO', result.stderr)
                else:
                    expected = value or DEFAULT_WSL_DISTRO
                    self.assertEqual(resolve_wsl_distro(env), expected)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, expected + '\n')
                    self.assertEqual(result.stderr, '')


class DispatchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='wsl selector ')
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.bin = self.folder / 'bin'
        self.bin.mkdir()
        self.audit = self.folder / 'audit.jsonl'
        tool = self.folder / 'tool.py'
        tool.write_text(TOOL)
        self.env = shell_env()
        python = Path(sys.executable).as_posix()
        for name in ('uname', 'cygpath', 'wsl', 'python3'):
            path = self.bin / name
            path.write_text(f'#!/bin/sh\nexec "{python}" "{tool.as_posix()}" {name} "$@"\n')
            path.chmod(0o755)
        self.env.update(PATH=str(self.bin) + os.pathsep + self.env['PATH'],
                        AUDIT=str(self.audit), EXPECTED_DISTRO='pe-ci')

    def records(self):
        return [json.loads(line) for line in self.audit.read_text().splitlines()] if self.audit.exists() else []

    def run_adapter(self, adapter, **updates):
        self.audit.unlink(missing_ok=True)
        env = dict(self.env, **updates)
        return subprocess.run(['sh', str(ROOT / adapter), 'run', 'space ; literal'],
                              env=env, cwd=ROOT, text=True, capture_output=True, timeout=20)

    def test_every_shell_adapter_default_empty_invalid_and_translation_refusal(self):
        for adapter in ADAPTERS:
            for value in (None, '', 'bad;name', 'pe-ci'):
                with self.subTest(adapter=adapter, value=value):
                    self.env.pop('ECHO_WSL_DISTRO', None)
                    if value is not None:
                        self.env['ECHO_WSL_DISTRO'] = value
                    expected = value or DEFAULT_WSL_DISTRO
                    result = self.run_adapter(adapter, DENY='1', EXPECTED_DISTRO=expected)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, '')
                    if value == 'bad;name':
                        self.assertIn('ECHO_WSL_DISTRO', result.stderr)
                        self.assertEqual(self.records(), [])
                    else:
                        self.assertIn(f"WSL distro '{expected}'", result.stderr)
                        record, = self.records()
                        self.assertEqual(record[0], 'wsl')
                        self.assertEqual(record[1][:4], ['-d', expected, '--exec', 'wslpath'])

    def test_every_shell_adapter_selected_success_and_guest_failure_preserve_argv(self):
        for adapter in ADAPTERS:
            for status in (0, 17):
                with self.subTest(adapter=adapter, status=status):
                    result = self.run_adapter(adapter, ECHO_WSL_DISTRO='pe-ci', GUEST_STATUS=str(status),
                                              HASSMIC_SRC='C:/source with spaces')
                    records = self.records()
                    self.assertTrue(records)
                    for name, args in records:
                        self.assertEqual(name, 'wsl')
                        self.assertEqual(args[:3], ['-d', 'pe-ci', '--exec'])
                    if status:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("WSL distro 'pe-ci'", result.stderr)
                    else:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        if adapter != ADAPTERS[-1]:
                            self.assertIn('space ; literal', records[-1][1])
                        self.assertEqual(result.stderr, '')

    def test_linux_hassmic_ignores_invalid_selector_before_native_usage_failure(self):
        result = subprocess.run(['sh', str(ROOT / ADAPTERS[0]), 'invalid'],
                                env=dict(self.env, ECHO_WSL_DISTRO='bad;name', PLATFORM='Linux'),
                                text=True, capture_output=True, timeout=20)
        # Use an invalid native command to avoid any build or host dependency.
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('ECHO_WSL_DISTRO', result.stderr)
        self.assertEqual(self.records(), [])

    def test_linux_adapters_ignore_invalid_selector_and_never_call_wsl(self):
        for adapter in ADAPTERS[2:]:
            with self.subTest(adapter=adapter):
                result = self.run_adapter(adapter, ECHO_WSL_DISTRO='bad;name', PLATFORM='Linux')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(self.records())
                self.assertTrue(all(name == 'python3' for name, _ in self.records()))


class PythonDispatchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('selector_sim_host',
            ROOT / 'crates/echo-lab/src/lab_run/sim/host.py')
        cls.host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.host)

    def test_invalid_rejects_before_child_and_defaults_refuse_without_forwarding(self):
        host = self.host
        for value in (None, '', *INVALID, 'pe-ci'):
            with self.subTest(value=value):
                env = {} if value is None else {'ECHO_WSL_DISTRO': value}
                stderr = io.StringIO()
                with mock.patch.dict(os.environ, env, clear=True), \
                        mock.patch.object(host.subprocess, 'Popen', side_effect=OSError('private')) as spawn, \
                        mock.patch.object(host, 'emit') as emit, contextlib.redirect_stderr(stderr):
                    self.assertEqual(host.launch_windows(), 1)
                emit.assert_called_once_with({'event': 'host.failed'})
                if value in INVALID:
                    spawn.assert_not_called()
                    self.assertIn('ECHO_WSL_DISTRO', stderr.getvalue())
                else:
                    self.assertEqual(spawn.call_count, 1)
                    self.assertEqual(spawn.call_args.args[0][:4],
                                     ['wsl', '-d', value or DEFAULT_WSL_DISTRO, '--exec'])
                    self.assertIn(f"WSL distro '{value or DEFAULT_WSL_DISTRO}'", stderr.getvalue())
                self.assertNotIn('private', stderr.getvalue())

    def test_guest_uses_same_resolved_name_and_preserves_finite_argv_and_status(self):
        host = self.host
        events = host.queue.Queue()
        events.put(('translated', '/checkout with spaces/host.py'))
        translation = mock.Mock()
        translation.close.return_value = 0
        owner = mock.Mock()
        owner.exited.return_value = True
        owner.close.return_value = 17
        with mock.patch.object(host, 'LauncherChild', return_value=owner), \
                mock.patch.object(host.subprocess, 'Popen') as spawn, \
                mock.patch.object(host.threading, 'Thread'), mock.patch.object(sys, 'argv', ['host.py', '--finite']):
            self.assertEqual(host.launcher_forward(events, translation, [], 'pe-ci'), 17)
        self.assertEqual(spawn.call_args.args[0], ['wsl', '-d', 'pe-ci', '--exec', 'python3',
                         '-u', '/checkout with spaces/host.py', '--finite'])
        owner.close.assert_called()
