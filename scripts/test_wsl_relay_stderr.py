"""T3 relay stderr delivery and forced collection through local child processes."""
import os
from pathlib import Path
import socket
import subprocess
import sys
import unittest
from unittest import mock

from wsl_relay import Connection

ROOT = Path(__file__).resolve().parent
SENTINEL = r'''
import socket, sys
from wsl_relay import Connection
peer, observer = socket.socketpair()
observer.settimeout(5)
connection = Connection(peer, [sys.executable, '-u', '-c',
    "import sys; print('relay-stderr-sentinel', file=sys.stderr, flush=True); sys.stdin.buffer.read()"])
try:
    connection.close()
    assert connection.proc.returncode == 0
    assert not connection.reader.is_alive()
    assert not connection.writer.is_alive()
    assert observer.recv(1) == b''
    assert connection.proc.stdin.closed and connection.proc.stdout.closed
finally:
    if connection.proc.poll() is None:
        connection.proc.kill()
        connection.proc.wait()
    observer.close()
print('exit=0 threads=collected peer=eof streams=closed')
'''


class RelayStderrTests(unittest.TestCase):
    def test_t3_guest_stderr_inherits_without_losing_collection_or_peer_eof(self):
        result = subprocess.run(
            [sys.executable, '-u', '-c', SENTINEL], capture_output=True,
            text=True, timeout=15,
            env=dict(os.environ, PYTHONPATH=str(ROOT), PYTHONDONTWRITEBYTECODE='1'))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'exit=0 threads=collected peer=eof streams=closed\n')
        self.assertEqual(result.stderr, 'relay-stderr-sentinel\n')

    def test_t3_expired_guest_wait_kills_collects_and_preserves_cleanup_error(self):
        peer, observer = socket.socketpair()
        self.addCleanup(observer.close)
        observer.settimeout(5)
        connection = Connection(peer, [sys.executable, '-u', '-c',
            'import threading; threading.Event().wait()'])
        self.addCleanup(self.collect, connection)
        actual_wait = connection.proc.wait
        calls = []

        def wait(timeout=None):
            calls.append(('wait', timeout))
            if timeout is not None:
                raise subprocess.TimeoutExpired(connection.proc.args, timeout)
            return actual_wait(timeout=5)

        actual_kill = connection.proc.kill

        def kill():
            calls.append(('kill', None))
            actual_kill()

        with mock.patch.object(connection.proc, 'wait', side_effect=wait), \
             mock.patch.object(connection.proc, 'kill', side_effect=kill):
            with self.assertRaisesRegex(RuntimeError, '^relay guest did not collect$'):
                connection.close()
        self.assertEqual(calls, [('wait', 3), ('kill', None), ('wait', None)])
        self.assertIsNotNone(connection.proc.returncode)
        self.assertNotEqual(connection.proc.returncode, 0)
        self.assertFalse(connection.reader.is_alive())
        self.assertFalse(connection.writer.is_alive())
        self.assertEqual(observer.recv(1), b'')
        self.assertTrue(connection.proc.stdin.closed)
        self.assertTrue(connection.proc.stdout.closed)
        for stream in (peer, connection.stop_read, connection.stop_write):
            self.assertEqual(stream.fileno(), -1)

    @staticmethod
    def collect(connection):
        if connection.proc.poll() is None:
            connection.proc.kill()
            connection.proc.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
