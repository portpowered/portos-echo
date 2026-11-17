"""T3 numeric protocol and lifecycle through native pipes and loopback sockets."""
import contextlib
import io
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

import wsl_controller
import wsl_relay
from wsl_relay_test_support import BOOTSTRAP, CONTROLLER

ROOT = Path(__file__).resolve().parent
PAYLOAD = bytes(range(256)) * 1024


class NumericRelayTests(unittest.TestCase):
    def launch(self):
        folder = tempfile.TemporaryDirectory(prefix='relay controller ')
        self.addCleanup(folder.cleanup)
        controller = Path(folder.name) / 'controller.py'
        controller.write_text(CONTROLLER)
        env = dict(os.environ, PYTHONPATH=str(ROOT), PYTHONDONTWRITEBYTECODE='1')
        proc = subprocess.Popen([sys.executable, '-u', '-c', BOOTSTRAP,
                                 str(ROOT / 'wsl_relay.py'), str(controller)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, env=env)
        self.addCleanup(self.dispose, proc)
        out, err = queue.Queue(), queue.Queue()
        for stream, events in ((proc.stdout, out), (proc.stderr, err)):
            threading.Thread(target=self.read, args=(stream, events), daemon=True).start()
        port = int(out.get(timeout=5))
        diagnostics = [err.get(timeout=5) for _ in range(3)]
        observer = int(next(line.split(': ')[1] for line in diagnostics if line.startswith('observer port:')))
        proc.host_pid = int(next(line.split(': ')[1] for line in diagnostics if line.startswith('host pid:')))
        return proc, out, err, port, observer

    @staticmethod
    def read(stream, events):
        for line in stream:
            events.put(line.strip())
        events.put(None)

    @staticmethod
    def dispose(proc):
        if not proc.stdin.closed:
            proc.stdin.close()
        try:
            proc.wait(timeout=5)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            proc.stdout.close()
            proc.stderr.close()

    def exchange(self, port):
        with socket.create_connection(('127.0.0.1', port), timeout=5) as peer:
            peer.sendall(PAYLOAD)
            peer.shutdown(socket.SHUT_WR)
            received = bytearray()
            while part := peer.recv(65536):
                received.extend(part)
        self.assertEqual(bytes(received), PAYLOAD[::-1])

    def finish(self, proc, out, err, port, observer):
        proc.stdin.close()
        self.assertEqual(proc.wait(timeout=5), 0)
        self.assertEqual(out.get(timeout=5), 'cleaned')
        self.assertIsNone(out.get(timeout=5))
        diagnostics = []
        while (line := err.get(timeout=5)) is not None:
            diagnostics.append(line)
        self.assertTrue(any(line.startswith('collected: ') for line in diagnostics), diagnostics)
        for endpoint in (port, observer):
            with self.assertRaises(OSError):
                socket.create_connection(('127.0.0.1', endpoint), timeout=1)

    def test_t3_numeric_observer_exact_duplex_and_reconnect(self):
        proc, out, err, port, observer = self.launch()
        for _ in range(2):
            self.exchange(port)
        with socket.create_connection(('127.0.0.1', observer), timeout=5) as peer:
            peer.shutdown(socket.SHUT_WR)
            self.assertEqual(peer.recv(65536), b'{"state":"controlled"}')
            self.assertEqual(peer.recv(1), b'')
        self.finish(proc, out, err, port, observer)

    def test_t3_host_kill_stop_restart_retains_endpoint_and_collects_generations(self):
        proc, out, err, port, observer = self.launch()
        self.exchange(port)
        with socket.create_connection(('127.0.0.1', port), timeout=5) as active:
            active.sendall(b'active at host death')
            previous = proc.host_pid
            for command in ('kill', 'restart', 'stop', 'restart'):
                proc.stdin.write(command + '\n')
                proc.stdin.flush()
                self.assertEqual(out.get(timeout=5), command)
                if command == 'restart':
                    current = int(err.get(timeout=5).split(': ')[1])
                    self.assertNotEqual(previous, current)
                    previous = current
                    self.exchange(port)
            self.assertEqual(active.recv(1), b'')
        self.finish(proc, out, err, port, observer)

    def test_t3_parallel_owner_and_unrelated_sentinel_survive_cleanup(self):
        first = self.launch()
        second = self.launch()
        sentinel = subprocess.Popen([sys.executable, '-c', 'import sys; sys.stdin.read()'], stdin=subprocess.PIPE)
        try:
            self.finish(*first)
            self.assertIsNone(sentinel.poll())
            self.exchange(second[3])
            self.finish(*second)
        finally:
            sentinel.stdin.close()
            self.assertEqual(sentinel.wait(timeout=5), 0)

    def test_t3_generation_replacement_reaps_active_child_and_threads(self):
        with socket.socket() as host:
            host.bind(('127.0.0.1', 0))
            host.listen()
            argv = [sys.executable, '-u', str(ROOT / 'wsl_relay.py'), '--guest',
                    str(host.getsockname()[1])]
            relay = wsl_relay.Relay(argv)
            try:
                port = int(relay.address.rsplit(':', 1)[1])
                with socket.create_connection(('127.0.0.1', port), timeout=5) as client:
                    with host.accept()[0] as guest:
                        client.sendall(b'active')
                        self.assertEqual(guest.recv(6), b'active')
                        old = list(relay.connections)
                        generation = relay.generation
                        relay.replace(argv)
                        self.assertGreater(relay.generation, generation)
                        self.assertEqual(relay.address, '127.0.0.1:' + str(port))
                        for connection in old:
                            self.assertEqual(connection.proc.poll(), 0)
                            self.assertFalse(connection.writer.is_alive())
                            self.assertFalse(connection.reader.is_alive())
            finally:
                relay.close()
            self.assertFalse(relay.thread.is_alive())

    def test_t3_guest_connection_refusal_is_eof_and_child_is_collected(self):
        with socket.socket() as unavailable:
            unavailable.bind(('127.0.0.1', 0))
            relay = wsl_relay.Relay([sys.executable, '-u', str(ROOT / 'wsl_relay.py'),
                                     '--guest', str(unavailable.getsockname()[1])])
            try:
                with socket.create_connection(('127.0.0.1', int(relay.address.rsplit(':', 1)[1])),
                                              timeout=5) as client:
                    self.assertEqual(client.recv(1), b'')
                connection, = relay.connections
            finally:
                relay.close()
            self.assertNotEqual(connection.proc.returncode, 0)
            self.assertFalse(connection.reader.is_alive())
            self.assertFalse(connection.writer.is_alive())


class ControllerFailureTests(unittest.TestCase):
    def test_empty_controller_cannot_report_success(self):
        proc = mock.Mock(stdin=io.StringIO(), stdout=io.StringIO(), stderr=io.StringIO())
        proc.wait.return_value = 0
        owner = wsl_controller.Controller(proc, ['guest'], False, queue.Queue())
        with mock.patch.object(sys, 'stdin', io.StringIO()), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(owner.run(), 1)
        self.assertEqual(output.getvalue(), '')
        self.assertTrue(proc.stdin.closed)

    def test_cancel_collects_without_success_marker(self):
        events = queue.Queue()
        events.put(('cancel', None))
        proc = mock.Mock(stdin=io.StringIO(), stdout=io.StringIO('12345\ncleaned\n'),
                         stderr=io.StringIO())
        proc.wait.return_value = 0
        relay = mock.Mock(address='127.0.0.1:23456')
        owner = wsl_controller.Controller(proc, ['guest'], False, events)
        with mock.patch.object(sys, 'stdin', io.StringIO()), \
                mock.patch.object(wsl_controller, 'Relay', return_value=relay), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(owner.run(), 1)
        self.assertEqual(output.getvalue(), '23456\n')
        self.assertTrue(proc.stdin.closed)
        relay.close.assert_called_once_with()

    def test_bind_failure_publishes_no_port(self):
        controller = wsl_controller.Controller(mock.Mock(), ['guest'], True, queue.Queue())
        for line in ('not a port\n', '-1\n', '65536\n', '12345\n'):
            with self.subTest(line=line), mock.patch.object(wsl_controller, 'Relay', side_effect=OSError('private')), \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                controller.port = None
                with self.assertRaises((ValueError, OSError)):
                    controller.output(line)
                self.assertEqual(output.getvalue(), '')
                self.assertEqual(controller.relays, {})

    def test_collect_attempts_all_owners_despite_failure(self):
        proc = mock.Mock()
        first, second, reader = mock.Mock(), mock.Mock(), mock.Mock()
        first.close.side_effect = RuntimeError('private')
        reader.is_alive.return_value = False
        with self.assertRaisesRegex(RuntimeError, '^controller cleanup failed$'):
            wsl_controller.collect(proc, {'host': first, 'observer': second}, [reader])
        second.close.assert_called_once_with()
        proc.wait.assert_called_once_with(timeout=4)
        reader.join.assert_called_once_with(timeout=0.5)
        proc.stdout.close.assert_called_once_with()
        proc.stderr.close.assert_called_once_with()

    def test_stdout_and_stderr_publication_failure_retains_owners(self):
        for stream in ('stdout', 'stderr'):
            with self.subTest(stream=stream):
                relay = mock.Mock(address='127.0.0.1:23456')
                owner = wsl_controller.Controller(mock.Mock(), ['guest'], True, queue.Queue())
                output = mock.Mock()
                output.write.side_effect = BrokenPipeError('private')
                owner.events.put((stream, '12345\n' if stream == 'stdout' else 'observer port: 12345\n'))
                reader = mock.Mock()
                reader.is_alive.return_value = False
                with mock.patch.object(wsl_controller, 'Relay', return_value=relay), \
                        mock.patch.object(wsl_controller.threading, 'Thread', return_value=reader), \
                        mock.patch.object(sys, stream, output):
                    with self.assertRaises(BrokenPipeError):
                        owner.run()
                self.assertEqual(list(owner.relays.values()), [relay])
                relay.close.assert_called_once_with()
                owner.proc.stdin.close.assert_called_once_with()
                owner.proc.stdout.close.assert_called_once_with()
                owner.proc.stderr.close.assert_called_once_with()

    def test_translation_and_spawn_failures_are_sanitized(self):
        for translation in (OSError('private'), '', '/controlled/relay.py'):
            with self.subTest(translation=translation):
                with mock.patch.object(sys, 'argv', ['relay', '--distro', 'controlled', '--controller', 'guest']), \
                        mock.patch.object(subprocess, 'check_output') as translate, \
                        mock.patch.object(subprocess, 'Popen', side_effect=OSError('private')) as spawn, \
                        contextlib.redirect_stderr(io.StringIO()) as error, \
                        contextlib.redirect_stdout(io.StringIO()) as output:
                    if isinstance(translation, OSError):
                        translate.side_effect = translation
                    else:
                        translate.return_value = translation
                    self.assertEqual(wsl_controller.main(), 1)
                self.assertEqual(output.getvalue(), '')
                self.assertEqual(error.getvalue(), 'WSL controller operation failed\n')
                self.assertEqual(spawn.call_count, int(translation == '/controlled/relay.py'))
