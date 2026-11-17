"""Selector units and T3 controlled dispatch: never forward to a real WSL."""
import contextlib
import importlib.util
import io
import itertools
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

from wsl_distro import DEFAULT_WSL_DISTRO, resolve_wsl_distro

ROOT = Path(__file__).resolve().parents[1]
ADAPTERS = ('scripts/hassmic-host.sh', 'scripts/build-echo-wsl.sh',
            'tests/wyoming_host.sh', 'tests/bridge_host.sh',
            'tests/hassmic_host_smoke.sh', 'crates/echo-bridge/tests/bridge_refusal/host.sh',
            'crates/echo-bridge/tests/bridge_observer/host.sh')
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
        for adapter, value, target in itertools.product(
                ADAPTERS, (None, '', 'bad;name', 'pe-ci'), ('', '/controlled/target')):
            with self.subTest(adapter=adapter, value=value, target=target):
                self.env.pop('ECHO_WSL_DISTRO', None)
                if value is not None:
                    self.env['ECHO_WSL_DISTRO'] = value
                expected = value or DEFAULT_WSL_DISTRO
                result = self.run_adapter(adapter, DENY='1', EXPECTED_DISTRO=expected,
                                          CARGO_TARGET_DIR=target)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, '')
                if value == 'bad;name':
                    self.assertIn('ECHO_WSL_DISTRO', result.stderr)
                    self.assertEqual(self.records(), [])
                else:
                    self.assertIn(f"WSL distro '{expected}'", result.stderr)
                    record, = self.records()
                    self.assertEqual(record[0], 'wsl')
                    # Absolute guest targets are cleaned before checkout translation.
                    command = 'sh' if adapter == ADAPTERS[1] and target else 'wslpath'
                    self.assertEqual(record[1][:4], ['-d', expected, '--exec', command])

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
                        if adapter != 'crates/echo-bridge/tests/bridge_refusal/host.sh':
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
        spec = importlib.util.spec_from_file_location('selector_device_supervisor',
            ROOT / 'crates/echo-lab/tests/device_support/supervisor.py')
        cls.supervisor = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.supervisor)

        import socket
        import threading
        cls.socket, cls.threading = socket, threading
        spec = importlib.util.spec_from_file_location('device_relay',
            ROOT / 'crates/echo-lab/tests/device_support/relay.py')
        cls.relay = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.relay)

    def test_supervisor_default_empty_invalid_and_translation_refusal(self):
        supervisor = self.supervisor
        for value in (None, '', *INVALID, 'pe-ci'):
            for failure in (OSError('private'), subprocess.CalledProcessError(29, 'wsl')):
                with self.subTest(value=value, failure=type(failure).__name__):
                    env = {} if value is None else {'ECHO_WSL_DISTRO': value}
                    stderr, stdout = io.StringIO(), io.StringIO()
                    with mock.patch.dict(os.environ, env, clear=True), \
                            mock.patch.object(supervisor, 'os', types.SimpleNamespace(name='nt')), \
                            mock.patch.object(subprocess, 'check_output', side_effect=failure) as translate, \
                            mock.patch.object(supervisor, 'forward') as launch, \
                            contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(stdout):
                        self.assertEqual(supervisor.main(), 1)
                    launch.assert_not_called()
                    self.assertEqual(stdout.getvalue(), '')
                    if value in INVALID:
                        translate.assert_not_called()
                        self.assertIn('ECHO_WSL_DISTRO', stderr.getvalue())
                    else:
                        self.assertEqual(translate.call_count, 1)
                        self.assertEqual(translate.call_args.args[0], [
                            'wsl', '-d', value or DEFAULT_WSL_DISTRO, '--exec', 'wslpath', '-u',
                            str(Path(supervisor.__file__).resolve())])
                        self.assertIn(f"WSL distro '{value or DEFAULT_WSL_DISTRO}'", stderr.getvalue())
                    self.assertNotIn('private', stderr.getvalue())

    def test_supervisor_selected_guest_preserves_argv_output_and_failure_status(self):
        for status in (0, 17, OSError('private')):
            with self.subTest(status=status):
                stderr, stdout = io.StringIO(), io.StringIO()

                def guest(*args):
                    if isinstance(status, OSError):
                        raise status
                    print('{"event":"host.ready","port":12345}')
                    return status

                with mock.patch.dict(os.environ, {'ECHO_WSL_DISTRO': 'pe-ci'}, clear=True), \
                        mock.patch.object(self.supervisor, 'os', types.SimpleNamespace(name='nt')), \
                        mock.patch.object(subprocess, 'check_output',
                                          return_value='/checkout with spaces ; literal/supervisor.py\n') as translate, \
                        mock.patch.object(self.supervisor, 'forward', side_effect=guest) as launch, \
                        contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(stdout):
                    self.assertEqual(self.supervisor.main(), 1 if isinstance(status, OSError) else status)
                self.assertEqual(translate.call_args.args[0][:4], ['wsl', '-d', 'pe-ci', '--exec'])
                launch.assert_called_once_with(['wsl', '-d', 'pe-ci', '--exec', 'python3', '-u',
                                               '/checkout with spaces ; literal/supervisor.py'],
                                              ['wsl', '-d', 'pe-ci', '--exec', 'python3', '-u',
                                               '/checkout with spaces ; literal/supervisor.py', '--relay'])
                self.assertEqual(stdout.getvalue(), '' if isinstance(status, OSError) else
                                 '{"event":"host.ready","port":12345}\n')
                self.assertEqual(stderr.getvalue(), "WSL distro 'pe-ci': operation failed\n" if status else '')

    def test_linux_supervisor_ignores_selector_and_runs_native_controller(self):
        native = types.SimpleNamespace(Host=type('Host', (), {}), main=mock.Mock(return_value=17))
        with mock.patch.dict(os.environ, {'ECHO_WSL_DISTRO': 'bad;name'}, clear=True), \
                mock.patch.object(self.supervisor, 'os', types.SimpleNamespace(name='posix')), \
                mock.patch.object(self.supervisor, 'launch_windows') as windows, \
                mock.patch.object(self.supervisor.importlib.util, 'spec_from_file_location') as spec, \
                mock.patch.object(self.supervisor.importlib.util, 'module_from_spec', return_value=native):
            self.assertEqual(self.supervisor.main(), 17)
        windows.assert_not_called()
        native.main.assert_called_once_with()
        self.assertEqual(spec.call_args.args[1], ROOT / 'crates/echo-lab/src/lab_run/sim/host.py')

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

    def exercise(self, remote_first=False, cancel=False):
        socket = self.socket
        errors = []
        received = []
        payload = bytes(range(256)) * 1024
        reply = payload[::-1]
        with socket.socket() as host:
            host.bind(('127.0.0.1', 0))
            host.listen()
            host.settimeout(5)
            port = host.getsockname()[1]

            def serve():
                try:
                    with host.accept()[0] as peer:
                        peer.settimeout(5)
                        if remote_first:
                            peer.sendall(reply)
                            peer.shutdown(socket.SHUT_WR)
                        data = bytearray()
                        while True:
                            chunk = peer.recv(65536)
                            if not chunk:
                                break
                            data.extend(chunk)
                        received.append(bytes(data))
                        if not remote_first and not cancel:
                            peer.sendall(reply)
                except BaseException as error:
                    errors.append(error)

            server = self.threading.Thread(target=serve)
            server.start()
            argv = [sys.executable, '-u', '-c',
                    'import sys; sys.path.insert(0, sys.argv[1]); from relay import guest; guest(int(sys.argv[2]))',
                    str(ROOT / 'crates/echo-lab/tests/device_support'), str(port)]
            relay = self.relay.Relay(argv)
            try:
                with socket.create_connection(('127.0.0.1', int(relay.address.split(':')[1])), timeout=5) as client:
                    if cancel:
                        client.sendall(payload)
                        # A response is unnecessary: close must collect an active guest.
                    elif remote_first:
                        data = bytearray()
                        while chunk := client.recv(65536):
                            data.extend(chunk)
                        self.assertEqual(bytes(data), reply)
                        client.sendall(payload)
                        client.shutdown(socket.SHUT_WR)
                    else:
                        client.sendall(payload)
                        client.shutdown(socket.SHUT_WR)
                        data = bytearray()
                        while chunk := client.recv(65536):
                            data.extend(chunk)
                        self.assertEqual(bytes(data), reply)
                if not cancel:
                    server.join(timeout=5)
            finally:
                relay.close()
                server.join(timeout=5)
            self.assertFalse(server.is_alive())
            self.assertEqual(errors, [])
            if not cancel:
                self.assertEqual(received, [payload])
            self.assertEqual(len(relay.connections), 1)
            for connection in relay.connections:
                self.assertIsNotNone(connection.proc.poll())
                self.assertFalse(connection.reader.is_alive())
                self.assertFalse(connection.writer.is_alive())
                self.assertTrue(connection.proc.stdin.closed)
                self.assertTrue(connection.proc.stdout.closed)

    def test_t3_exact_duplex_and_client_half_close(self):
        self.exercise()

    def test_t3_guest_half_close_still_accepts_client_bytes(self):
        self.exercise(remote_first=True)

    def test_t3_cancel_collects_active_relay_process_and_streams(self):
        self.exercise(cancel=True)
