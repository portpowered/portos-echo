"""T1 lifecycle accounting and T3 prebuilt pinned-host cleanup on loopback.

Run explicitly with python3 -m unittest discover -s tests -p 'test_host_teardown*.py' -v.
No build, hardware, real credentials, retries, or non-loopback clients.
"""
import contextlib
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import bridge_host
import wyoming_host
from wyoming_host import Host, ROOT


class StopAccounting(unittest.TestCase):
    """T1: faults are in-memory and never signal a real PID."""

    def test_coordination_timeout_collects_waiter_and_closes_lock(self):
        waiter, owner = mock.Mock(), mock.Mock()
        waiter.wait.side_effect = [subprocess.TimeoutExpired('coordination', 3.5), 0]
        waiter.poll.return_value = None
        with mock.patch('wyoming_host.os.open', return_value=43), \
                mock.patch('wyoming_host.os.fdopen', return_value=owner), \
                mock.patch('wyoming_host._LockWaiter', return_value=waiter) as launch, \
                mock.patch('wyoming_host.time.monotonic', return_value=16.5):
            with self.assertRaises(subprocess.TimeoutExpired):
                with wyoming_host._allocation_guard(20):
                    self.fail('expired acquisition entered the handoff')
        self.assertEqual(waiter.wait.call_args_list,
                         [mock.call(timeout=3.5), mock.call()])
        waiter.kill.assert_called_once()
        owner.close.assert_called_once()
        self.assertEqual(launch.call_args.kwargs['pass_fds'], (43,))

    def host(self, status=None):
        host = Host.__new__(Host)
        host.proc = mock.Mock(pid=123)
        host.proc.poll.return_value = status
        host.proc.returncode = status
        host.proc.stderr = io.BytesIO()
        return host

    def test_stop_collects_once_and_never_signals_stale_identity(self):
        host = self.host()
        proc = host.proc
        with mock.patch('wyoming_host.os.killpg') as kill:
            host.stop()
            host.stop()
        kill.assert_called_once_with(123, signal.SIGKILL)
        proc.wait.assert_called_once()
        self.assertTrue(proc.stderr.closed)
        self.assertIsNone(host.proc)

    def test_already_collected_child_is_not_signaled(self):
        host = self.host(status=7)
        proc = host.proc
        with mock.patch('wyoming_host.os.killpg') as kill:
            host.stop()
        kill.assert_not_called()
        proc.wait.assert_called_once()
        self.assertTrue(proc.stderr.closed)

    def test_group_disappears_before_signal_but_child_is_collected(self):
        host = self.host()
        proc = host.proc
        with mock.patch('wyoming_host.os.killpg', side_effect=ProcessLookupError):
            host.stop()
        proc.wait.assert_called_once()
        self.assertTrue(proc.stderr.closed)
        self.assertIsNone(host.proc)

    def test_signal_error_closes_pipe_without_claiming_collection(self):
        host = self.host()
        proc = host.proc
        with mock.patch('wyoming_host.os.killpg', side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                host.stop()
        self.assertTrue(proc.stderr.closed)
        self.assertIs(host.proc, proc)
        proc.wait.assert_not_called()

    def test_timeout_closes_pipe_preserves_ownership_and_is_not_swallowed(self):
        host = self.host()
        proc = host.proc
        proc.wait.side_effect = subprocess.TimeoutExpired('owned-host', 2)
        with mock.patch.object(host, 'stop_status', create=True) as diagnostic:
            with mock.patch('wyoming_host.os.killpg'):
                with self.assertRaises(subprocess.TimeoutExpired):
                    host.stop()
        self.assertTrue(proc.stderr.closed)
        self.assertIs(host.proc, proc)
        diagnostic.assert_called_once_with(proc)

    def test_signaling_consumes_the_same_two_second_budget(self):
        host = self.host()
        proc = host.proc
        with mock.patch('wyoming_host.time.monotonic', side_effect=[10, 11.5]):
            with mock.patch('wyoming_host.os.killpg'):
                host.stop()
        proc.wait.assert_called_once_with(timeout=0.5)

    def test_restart_cannot_overwrite_uncollected_child(self):
        host = self.host()
        proc = host.proc
        with self.assertRaisesRegex(RuntimeError, 'still owned'):
            host.start()
        self.assertIs(host.proc, proc)

    def test_bridge_closes_playback_even_when_host_stop_raises(self):
        host = mock.Mock(port=123, folder=Path('unopened-test-folder'))
        host.stop.side_effect = subprocess.TimeoutExpired('owned-host', 2)
        sink = mock.Mock()
        output = io.StringIO()
        with mock.patch.object(bridge_host, 'Host', return_value=host):
            with mock.patch.object(bridge_host, 'playback', return_value=sink):
                with mock.patch('sys.stdin', io.StringIO('arm\n')):
                    with contextlib.redirect_stdout(output):
                        with self.assertRaises(subprocess.TimeoutExpired):
                            bridge_host.main()
        sink.close.assert_called_once()
        self.assertEqual(output.getvalue(), '123\narmed\n')


class PinnedHost(unittest.TestCase):
    """T3: real OS waits and sockets, consuming the lane-local pinned build."""

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix='host-reap-t3-')
        self.addCleanup(self.folder.cleanup)
        self.host = Host(self.folder.name)
        self.addCleanup(self.host.stop)

    def connect(self):
        conn = socket.create_connection(('127.0.0.1', self.host.port), timeout=2)
        self.addCleanup(conn.close)
        conn.sendall(b'{"type":"describe"}\n')
        self.assertIn(b'"info"', conn.recv(65536))
        return conn

    def test_host_outlives_start_worker_and_stop_collects_child(self):
        with ThreadPoolExecutor(max_workers=1) as pool:
            pool.submit(self.host.start).result(timeout=20)
        # The spawning owner, unlike the executor worker, remains until exit.
        self.connect()
        proc = self.host.proc
        self.host.stop()
        self.assertEqual(proc.returncode, -signal.SIGKILL)
        self.assertIsNone(self.host.proc)
        self.assertIsNone(self.host.staged)

    def test_actual_identity_collect_restart_and_socket_closure(self):
        self.host.start()
        proc = self.host.proc
        executable = Path(f'/proc/{proc.pid}/exe').resolve()
        self.assertEqual(executable.name, 'hassmic-host')
        self.assertNotIn(Path('/mnt'), executable.parents)
        self.assertEqual(Path(f'/proc/{proc.pid}/cwd').resolve(), executable.parent)
        pinned = subprocess.check_output(
            ['sh', str(ROOT / 'scripts/hassmic-host.sh'), 'path'], text=True).strip()
        self.assertEqual(hashlib.sha256(executable.read_bytes()).digest(),
                         hashlib.sha256(Path(pinned).read_bytes()).digest())
        self.assertEqual(os.getpgid(proc.pid), proc.pid)
        self.assertEqual(os.getsid(proc.pid), proc.pid)
        conn = self.connect()
        self.host.stop()
        self.assertEqual(proc.returncode, -signal.SIGKILL)
        self.assertTrue(proc.stderr.closed)
        self.assertFalse(executable.parent.exists())
        self.assertEqual(conn.recv(65536), b'')
        with self.assertRaises(OSError):
            socket.create_connection(('127.0.0.1', self.host.port), timeout=2)
        with mock.patch('wyoming_host.os.killpg') as kill:
            self.host.stop()
        kill.assert_not_called()
        self.host.deadline = 0
        self.host.start()
        self.assertNotEqual(self.host.proc.pid, proc.pid)
        self.assertNotEqual(Path(f'/proc/{self.host.proc.pid}/exe').resolve(), executable)
        self.connect()
        advertised = self.host.port
        self.host.stop()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.host.command('restart')
        self.assertEqual(output.getvalue(), 'restart\n')
        self.assertEqual(self.host.port, advertised)
        self.connect()

    def test_staged_copy_mismatch_refuses_launch_and_removes_private_directory(self):
        original = shutil.copy2
        destinations = []

        def corrupt(source, destination):
            original(source, destination)
            destination.write_bytes(b'corrupt staged artifact')
            destinations.append(destination)

        with mock.patch('wyoming_host.shutil.copy2', side_effect=corrupt):
            with mock.patch('wyoming_host.subprocess.Popen', wraps=subprocess.Popen) as launch:
                with self.assertRaisesRegex(RuntimeError, 'stage-hash-mismatch'):
                    self.host.start()
        self.assertEqual(launch.call_count, 1)  # Only the pin resolver; no host.
        self.assertEqual(launch.call_args.args[0][-1], 'path')
        self.assertIsNone(self.host.proc)
        self.assertIsNone(self.host.staged)
        self.assertFalse(destinations[0].parent.exists())

    def test_failed_exec_removes_private_directory(self):
        original = subprocess.Popen
        directories = []

        def reject_host(args, **kwargs):
            if args[-1] == 'path':
                return original(args, **kwargs)
            directories.append(Path(args[0]).parent)
            raise PermissionError('launch rejected')

        with mock.patch('wyoming_host.subprocess.Popen', side_effect=reject_host):
            with self.assertRaisesRegex(RuntimeError, 'startup-error'):
                self.host.start()
        self.assertIsNone(self.host.proc)
        self.assertIsNone(self.host.staged)
        self.assertFalse(directories[0].exists())

    def test_eof_and_intentional_startup_failure_exact_transcripts(self):
        for arguments, marker in [([], 'cleaned'),
                                  (['--startup-failure'], 'startup-failed-cleaned')]:
            with self.subTest(arguments=arguments):
                result = subprocess.run(
                    [sys.executable, str(ROOT / 'tests/wyoming_host.py'), *arguments],
                    input='', capture_output=True, text=True, timeout=25)
                self.assertEqual(result.returncode, 0, result.stderr)
                lines = result.stdout.splitlines()
                self.assertEqual(lines[-1], marker)
                self.assertEqual(len(lines), 2 if not arguments else 1)
                self.assertEqual(result.stderr, '')

    def test_collected_host_does_not_signal_reused_identity(self):
        self.host.start()
        proc = self.host.proc
        proc.kill()
        self.assertEqual(proc.wait(timeout=2), -signal.SIGKILL)
        with mock.patch('wyoming_host.os.killpg') as kill:
            self.host.stop()
        kill.assert_not_called()
        self.assertTrue(proc.stderr.closed)

    def test_two_stop_commands_then_eof_have_exact_acknowledgements(self):
        result = subprocess.run(
            [sys.executable, str(ROOT / 'tests/wyoming_host.py')],
            input='stop\nstop\n', capture_output=True, text=True, timeout=25)
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertTrue(lines[0].isdecimal())
        self.assertEqual(lines[1:], ['stop', 'stop', 'cleaned'])
        self.assertEqual(result.stderr, '')

    def test_bridge_active_writer_eof_closes_reader_and_collects_host(self):
        sinks, processes = [], []
        original = bridge_host.playback

        def sink(folder):
            observer = original(folder)
            sinks.append(observer)
            return observer

        owned = []

        def host(folder):
            instance = Host(folder)
            owned.append(instance)
            return instance

        output = io.StringIO()
        # A separate open descriptor keeps the writer active after the input
        # iterator ends; close it only after observing controller cleanup.
        writers = []

        def active_commands():
            yield 'arm\n'
            observer = sinks[0]
            writers.append(observer.path.open('wb', buffering=0))
            writers[0].write(bytes(64))
            with observer.condition:
                self.assertTrue(observer.condition.wait_for(
                    lambda: len(observer.data) == 64, timeout=2))
            processes.append(owned[0].proc)

        try:
            with mock.patch.object(bridge_host, 'Host', side_effect=host):
                with mock.patch.object(bridge_host, 'playback', side_effect=sink):
                    with mock.patch('sys.stdin', active_commands()):
                        with contextlib.redirect_stdout(output):
                            bridge_host.main()
            self.assertEqual(processes[0].returncode, -signal.SIGKILL)
            self.assertTrue(processes[0].stderr.closed)
            self.assertIsNone(owned[0].proc)
            self.assertFalse(sinks[0].thread.is_alive())
            self.assertEqual(sinks[0].data, bytes(64))
            self.assertIsNone(sinks[0].anchor)
            with self.assertRaises(OSError):
                os.fstat(sinks[0].fd)
            self.assertEqual(output.getvalue().splitlines()[1:], ['armed', 'cleaned'])
        finally:
            for writer in writers:
                writer.close()

    def test_other_host_and_unrelated_sentinel_survive_stop(self):
        sentinel = subprocess.Popen(['cat'], stdin=subprocess.PIPE,
                                    stdout=subprocess.DEVNULL, start_new_session=True)
        try:
            with tempfile.TemporaryDirectory(prefix='other-host-t3-') as folder:
                other = Host(folder)
                try:
                    self.host.start()
                    other.start()
                    former_port = self.host.port
                    self.host.stop()
                    with socket.socket() as unrelated:
                        unrelated.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                        unrelated.bind(('127.0.0.1', former_port))
                        unrelated.listen()
                        unrelated.setblocking(False)
                        with mock.patch('wyoming_host.os.killpg') as kill, \
                                mock.patch('socket.create_connection') as probe:
                            self.host.stop()
                            self.assertIn('status=None', str(self.host.failure('stopped')))
                        kill.assert_not_called()
                        probe.assert_not_called()
                        self.host.start()
                        self.assertNotEqual(self.host.port, former_port)
                        self.connect()
                        self.assertEqual(unrelated.getsockname()[1], former_port)
                        with self.assertRaises(BlockingIOError):
                            unrelated.accept()
                    self.assertIsNone(sentinel.poll())
                    self.assertIsNone(other.proc.poll())
                    with socket.create_connection(('127.0.0.1', other.port), timeout=2) as conn:
                        conn.sendall(b'{"type":"describe"}\n')
                        self.assertIn(b'"info"', conn.recv(65536))
                finally:
                    other.stop()
        finally:
            sentinel.stdin.close()
            self.assertEqual(sentinel.wait(timeout=2), 0)

    def test_bridge_playback_descriptors_close_on_eof_and_invalid_command(self):
        for commands, fails in [('arm\n', False), ('arm\ninvalid\n', True)]:
            with self.subTest(commands=commands):
                sinks = []
                hosts = []
                original = bridge_host.playback

                def sink(folder):
                    observer = original(folder)
                    sinks.append(observer)
                    return observer

                def host(folder):
                    owned = Host(folder)
                    hosts.append(owned)
                    return owned

                output = io.StringIO()
                with mock.patch.object(bridge_host, 'Host', side_effect=host):
                    with mock.patch.object(bridge_host, 'playback', side_effect=sink):
                        with mock.patch('sys.stdin', io.StringIO(commands)):
                            with contextlib.redirect_stdout(output):
                                if fails:
                                    with self.assertRaises(ValueError):
                                        bridge_host.main()
                                else:
                                    bridge_host.main()
                self.assertIsNone(hosts[0].proc)
                observer = sinks[0]
                self.assertFalse(observer.thread.is_alive())
                for fd in [observer.fd, observer.anchor]:
                    with self.assertRaises(OSError):
                        os.fstat(fd)
                self.assertEqual(output.getvalue().splitlines()[1:],
                                 ['armed'] if fails else ['armed', 'cleaned'])


class ControllerFailureBoundary(unittest.TestCase):
    """T3: actual interpreter exit status and stdout on failed cleanup."""

    def test_ordinary_startup_exit_reports_failure_after_collecting_resources(self):
        runner = '''
import sys
from unittest import mock
sys.path.insert(0, sys.argv[1])
import wyoming_host
original = wyoming_host.subprocess.Popen
children = []
def rejected(args, **kwargs):
    if args[-1] == 'path':
        return original(args, **kwargs)
    proc = original([*args, '--invalid-test-option'], **kwargs)
    children.append(proc)
    return proc
try:
    with mock.patch.object(wyoming_host.subprocess, 'Popen', side_effect=rejected):
        wyoming_host.main()
finally:
    assert len(children) == 1
    assert children[0].returncode is not None
    assert children[0].stderr.closed
    print('startup-child-collected; stderr-closed', file=sys.stderr)
'''
        result = subprocess.run(
            [sys.executable, '-c', runner, str(ROOT / 'tests')],
            input='', capture_output=True, text=True, timeout=25)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, '')
        self.assertIn('host readiness failed:', result.stderr)
        self.assertIn('startup-child-collected; stderr-closed', result.stderr)

    FAILURE_RUNNER = '''
import os, signal, subprocess, sys
from unittest import mock
sys.path.insert(0, sys.argv[1])
import bridge_host
from wyoming_host import Host
hosts, sinks = [], []
real_playback = bridge_host.playback
real_killpg = os.killpg
def host(folder):
    instance = Host(folder)
    hosts.append(instance)
    return instance
def playback(folder):
    instance = real_playback(folder)
    sinks.append(instance)
    return instance
fault = PermissionError if sys.argv[2] == 'signal' else None
try:
    with mock.patch.object(bridge_host, 'Host', side_effect=host):
        with mock.patch.object(bridge_host, 'playback', side_effect=playback):
            with mock.patch('wyoming_host.os.killpg', side_effect=fault):
                bridge_host.main()
    raise AssertionError('failed cleanup claimed success')
except (PermissionError, subprocess.TimeoutExpired):
    proc = hosts[0].proc
    try:
        assert proc is not None and proc.poll() is None
        assert proc.stderr.closed
        observer = sinks[0]
        assert not observer.thread.is_alive()
        for fd in (observer.fd, observer.anchor):
            try:
                os.fstat(fd)
            except OSError:
                continue
            raise AssertionError('observer descriptor leaked')
        print('failed-owned-child; stderr-and-observer-closed', file=sys.stderr)
    finally:
        # Emergency cleanup also runs if an observation fails.
        if proc is not None:
            real_killpg(proc.pid, signal.SIGKILL)
            assert proc.wait(timeout=2) == -signal.SIGKILL
    sys.exit(1)
'''

    def test_failed_signal_and_real_wait_deadline_exit_nonzero_without_cleaned(self):
        for fault in ['signal', 'timeout']:
            with self.subTest(fault=fault):
                result = subprocess.run(
                    [sys.executable, '-c', self.FAILURE_RUNNER, str(ROOT / 'tests'), fault],
                    input='arm\n', capture_output=True, text=True, timeout=25)
                self.assertEqual(result.returncode, 1, result.stderr)
                lines = result.stdout.splitlines()
                self.assertTrue(lines[0].isdecimal())
                self.assertEqual(lines[1:], ['armed'])
                self.assertIn('failed-owned-child; stderr-and-observer-closed', result.stderr)


if __name__ == '__main__':
    unittest.main()
