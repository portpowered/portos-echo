"""T1 lifecycle accounting and T3 prebuilt pinned-host cleanup on loopback.

Run explicitly with python3 -m unittest discover -s tests -p 'test_host_teardown*.py' -v.
No build, hardware, real credentials, retries, or non-loopback clients.
"""
import contextlib
import io
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import bridge_host
from wyoming_host import Host, ROOT


class StopAccounting(unittest.TestCase):
    """T1: faults are in-memory and never signal a real PID."""

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

    def test_actual_identity_collect_restart_and_socket_closure(self):
        self.host.start()
        proc = self.host.proc
        self.assertEqual(Path(f'/proc/{proc.pid}/exe').resolve().name, 'hassmic-host')
        self.assertEqual(os.getpgid(proc.pid), proc.pid)
        self.assertEqual(os.getsid(proc.pid), proc.pid)
        conn = self.connect()
        self.host.stop()
        self.assertEqual(proc.returncode, -signal.SIGKILL)
        self.assertTrue(proc.stderr.closed)
        self.assertEqual(conn.recv(65536), b'')
        with self.assertRaises(OSError):
            socket.create_connection(('127.0.0.1', self.host.port), timeout=2)
        with mock.patch('wyoming_host.os.killpg') as kill:
            self.host.stop()
        kill.assert_not_called()
        self.host.deadline = 0
        self.host.start()
        self.assertNotEqual(self.host.proc.pid, proc.pid)
        self.connect()

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

    def test_other_host_and_unrelated_sentinel_survive_stop(self):
        sentinel = subprocess.Popen(['cat'], stdin=subprocess.PIPE,
                                    stdout=subprocess.DEVNULL, start_new_session=True)
        try:
            with tempfile.TemporaryDirectory(prefix='other-host-t3-') as folder:
                other = Host(folder)
                try:
                    self.host.start()
                    other.start()
                    self.host.stop()
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


if __name__ == '__main__':
    unittest.main()
