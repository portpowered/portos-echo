"""T1 readiness accounting and T3 owned-process startup on synthetic loopback.

Integration consumes the prebuilt pin; no build, retries or sleep synchronization.
"""
import collections
import concurrent.futures
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

import bridge_host
import wyoming_host
from wyoming_host import Host, ROOT


# Loading the observer must not change bridge_host's normal controller binding.
spec = importlib.util.spec_from_file_location(
    'startup_observer', ROOT / 'crates/echo-bridge/tests/bridge_observer/host.py')
observer = importlib.util.module_from_spec(spec)
with mock.patch.object(bridge_host, 'Host', Host):
    spec.loader.exec_module(observer)
ObservedHost = observer.ObservedHost
KINDS = (Host, ObservedHost)


def component(kind):
    host = kind.__new__(kind)
    host.proc = mock.Mock()
    host.proc.poll.return_value = None
    host.proc.stderr.fileno.return_value = 42
    host.log = bytearray()
    host.tail = bytearray()
    host.truncated = False
    host.guard = threading.RLock()
    host.observed = collections.deque(maxlen=8192)
    host.deadline = 20
    host.reservation = mock.Mock()
    return host


class Readiness(unittest.TestCase):
    """T1: explicit component doubles, no descriptors or sockets opened."""

    def setUp(self):
        self.enterContext(mock.patch('wyoming_host._allocation_guard',
                                     side_effect=lambda deadline: contextlib.nullcontext()))

    def test_split_marker_preserves_trailing_record_for_next_reader(self):
        for kind in KINDS:
            with self.subTest(kind=kind.__name__):
                host = component(kind)
                with mock.patch('wyoming_host.time.monotonic', return_value=1), \
                        mock.patch('wyoming_host.select.select', return_value=([42], [], [])), \
                        mock.patch('wyoming_host.os.read', side_effect=[
                            b'wyom', b'ing on 123\nsatellite: running\n']) as read:
                    host.logged(b'wyoming on ')
                    self.assertEqual(host.log, b'123\nsatellite: running\n')
                    host.logged(b'satellite: running')
                    self.assertEqual(read.call_count, 2)
                    self.assertEqual(host.log, b'\n')
                if kind is ObservedHost:
                    self.assertEqual(bytes(host.observed),
                                     b'wyoming on 123\nsatellite: running\n')

    def test_exited_child_drains_buffered_reason_and_cannot_claim_ready(self):
        for kind in KINDS:
            with self.subTest(kind=kind.__name__):
                host = component(kind)
                host.proc.poll.return_value = 7
                with mock.patch('wyoming_host.select.select', return_value=([42], [], [])), \
                        mock.patch('wyoming_host.os.read', side_effect=[
                            b'wyoming on 123\nsynthetic startup rejected\n', b'']):
                    with self.assertRaisesRegex(RuntimeError,
                                                'reason=child-exited status=7') as error:
                        host.logged(b'wyoming on ')
                self.assertIn('synthetic startup rejected', str(error.exception))

    def test_eof_alive_is_prompt_and_not_mislabeled_as_cleanup_signal(self):
        for kind in KINDS:
            with self.subTest(kind=kind.__name__):
                host = component(kind)
                with mock.patch('wyoming_host.time.monotonic', return_value=1) as clock, \
                        mock.patch('wyoming_host.select.select', return_value=([42], [], [])), \
                        mock.patch('wyoming_host.os.read', return_value=b'') as read:
                    with self.assertRaisesRegex(RuntimeError, 'reason=stderr-eof status=None'):
                        host.logged(b'wyoming on ')
                self.assertEqual(read.call_count, 1)
                self.assertEqual(clock.call_count, 1)

    def test_deadline_retains_alive_status_and_existing_absolute_budget(self):
        host = component(Host)
        with mock.patch('wyoming_host.time.monotonic', return_value=20), \
                mock.patch('wyoming_host.select.select', return_value=([], [], [])) as ready:
            with self.assertRaisesRegex(RuntimeError, 'reason=deadline status=None'):
                host.logged(b'wyoming on ')
        ready.assert_called_once_with([host.proc.stderr], [], [], 0)

    def test_tail_is_bounded_and_redacts_partial_private_and_malformed_records(self):
        host = component(Host)
        host.record(b'private-canary' + b'x' * 9000)
        host.record(b'\nBearer synthetic-canary\n/tmp/canary\nC:\\canary\n'
                    b'api_key=canary\ncontrol\x1bcanary\ninvalid\xffcanary\n'
                    b'synthetic startup rejected\nunfinished-canary')
        detail = str(host.failure('child-exited', 7))
        self.assertLessEqual(len(host.tail), 8192)
        self.assertNotIn('canary', detail)
        self.assertIn('synthetic startup rejected\n<redacted>', detail)
        self.assertEqual(detail.count('<redacted>'), 7)
        self.assertLess(len(detail), 8192)

    def test_cleanup_fault_keeps_original_startup_evidence_and_ownership(self):
        host = component(Host)
        host.proc = None
        host.folder = Path('unopened-synthetic-folder')
        host.port = 123
        proc = mock.Mock()
        proc.poll.return_value = 7
        original = host.failure('child-exited', 7)

        def launch(*args, **kwargs):
            host.proc = proc
            return proc

        with mock.patch.object(host, 'stage', return_value=Path('host')), \
                mock.patch('wyoming_host.subprocess.Popen', side_effect=launch), \
                mock.patch.object(host, 'logged', side_effect=original), \
                mock.patch.object(host, 'stop', side_effect=PermissionError):
            with self.assertRaisesRegex(RuntimeError, 'reason=cleanup-failed') as error:
                host.start()
        self.assertIs(error.exception.__cause__, original)
        self.assertIs(host.proc, proc)

    def test_exec_error_keeps_errno_without_exposing_exception_paths(self):
        host = component(Host)
        host.proc = None
        host.folder = Path('unopened-synthetic-folder')
        host.port = 123
        with mock.patch.object(host, 'stage', return_value=Path('host')), \
                mock.patch('wyoming_host.subprocess.Popen',
                           side_effect=OSError(26, 'secret canary', '/tmp/canary')), \
                mock.patch.object(host, 'stop') as stop:
            with self.assertRaisesRegex(RuntimeError, 'startup-error errno=26') as error:
                host.start()
        stop.assert_called_once()
        self.assertNotIn('canary', str(error.exception))
        self.assertIn('status=None', str(error.exception))


class StartupProcesses(unittest.TestCase):
    """T3: real prebuilt pin, OS fork/exec, pipes and synthetic IPv4 clients."""

    PEER = r'''
import contextlib, fcntl, json, os, socket, sys, tempfile
from unittest import mock
sys.path.insert(0, sys.argv[1])
import test_host_startup as suite
role = sys.argv[2]
if role == 'observer' and sys.argv[4] == 'handoff':
    with open(f'/tmp/wyoming-t3-bind-{os.getuid()}.lock', 'rb') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(json.dumps({'excluded': True}), flush=True)
        else:
            raise AssertionError('handoff was not coordinated across processes')
with tempfile.TemporaryDirectory(prefix='endpoint-peer-') as folder:
    bind = socket.socket.bind
    calls = 0
    def allocate(sock, address):
        global calls
        calls += 1
        if role == 'observer' and calls == 2:
            try:
                return bind(sock, ('127.0.0.1', int(sys.argv[3])))
            except OSError as error:
                assert error.errno == 98
        return bind(sock, address)
    with mock.patch.object(socket.socket, 'bind', allocate):
        host = (suite.ObservedHost if role == 'observer' else suite.Host)(folder)
    detail = {'selected': host.port, 'pid': os.getpid()}
    if role == 'observer':
        detail.update(diagnostic=host.listener.getsockname()[1],
                      descriptor=host.listener.fileno())
    print(json.dumps(detail), flush=True)
    try:
        assert sys.stdin.readline() == 'start\n'
        launch = suite.wyoming_host.subprocess.Popen
        def handoff(args, **kwargs):
            if args[-1] != 'path':
                assert host.reservation is None
                print(json.dumps({'handoff': host.port}), flush=True)
                assert sys.stdin.readline() == 'bind\n'
            return launch(args, **kwargs)
        try:
            with mock.patch('wyoming_host.subprocess.Popen',
                            handoff if role == 'handoff' else launch):
                host.start()
            print(json.dumps({'ready': host.port}), flush=True)
        except RuntimeError as error:
            print(json.dumps({'failure': str(error)}), flush=True)
        for command in sys.stdin:
            if command.strip() == 'stop':
                host.stop()
                print('stopped', flush=True)
    finally:
        host.stop()
        if role == 'observer':
            with contextlib.suppress(OSError):
                host.listener.shutdown(socket.SHUT_RDWR)
            host.listener.close()
'''

    def test_independent_observer_cannot_take_selected_core_endpoint(self):
        self.independent(False)

    def test_independent_allocator_is_excluded_until_child_binds(self):
        self.independent(True)

    def independent(self, handoff):
        # Force the kernel allocator's contested choice, without a port probe.
        # Each exec owns its descriptors; both real pinned children start once.
        peers = []

        def peer(role, port=0):
            proc = subprocess.Popen(
                [sys.executable, '-c', self.PEER, str(ROOT / 'tests'), role, str(port),
                 'handoff' if handoff else 'preparation'],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True)
            peers.append(proc)
            return proc

        def row(proc):
            self.assertTrue(wyoming_host.select.select([proc.stdout], [], [], 20)[0])
            line = proc.stdout.readline()
            self.assertTrue(line, 'controller EOF')
            return json.loads(line)

        try:
            first = peer('handoff' if handoff else 'base')
            selected = row(first)['selected']
            if handoff:
                first.stdin.write('start\n')
                first.stdin.flush()
                self.assertEqual(row(first), {'handoff': selected})
            second = peer('observer', selected)
            if handoff:
                self.assertEqual(row(second), {'excluded': True})
                first.stdin.write('bind\n')
                first.stdin.flush()
            owner = row(second)
            for proc in peers[1:] if handoff else peers:
                proc.stdin.write('start\n')
                proc.stdin.flush()
            outcomes = [row(proc) for proc in peers]
            print(f'core={selected} contender={owner} outcomes={outcomes}', file=sys.stderr)
            self.assertEqual(outcomes, [{'ready': selected}, {'ready': owner['selected']}])
            self.assertNotIn(owner['diagnostic'], (selected, owner['selected']))
            for port in (selected, owner['selected']):
                self.describe(mock.Mock(port=port))
            first.stdin.write('stop\n')
            first.stdin.flush()
            self.assertEqual(first.stdout.readline(), 'stopped\n')
            self.describe(mock.Mock(port=owner['selected']))
        finally:
            for proc in peers:
                proc.stdin.close()
                proc.wait(timeout=3)
                proc.stdout.close()
                diagnostic = proc.stderr.read()
                proc.stderr.close()
                self.assertEqual(proc.returncode, 0, diagnostic)

    def host(self, kind):
        folder = tempfile.TemporaryDirectory(prefix='startup-t3-')
        self.addCleanup(folder.cleanup)
        with contextlib.redirect_stderr(io.StringIO()):
            host = kind(folder.name)
        if kind is ObservedHost:
            def close_observer():
                with contextlib.suppress(OSError):
                    host.listener.shutdown(socket.SHUT_RDWR)
                host.listener.close()
            self.addCleanup(close_observer)
        self.addCleanup(host.stop)
        return host

    def describe(self, host):
        with socket.create_connection(('127.0.0.1', host.port), timeout=2) as conn:
            conn.sendall(b'{"type":"describe"}\n')
            self.assertIn(b'"info"', conn.recv(65536))

    def test_copy_and_fork_cannot_overlap_writable_executable(self):
        # Rendezvous, not load/timing: a sibling fork inherits A's open writer.
        # That child stays alive until A's exec result, deterministically causing
        # ETXTBSY on original Host.start. The guard prevents that inheritance.
        for kind in KINDS:
            with self.subTest(kind=kind.__name__):
                first, sibling = self.host(kind), self.host(kind)
                writer_open, second_attempt = threading.Event(), threading.Event()
                release_read, release_write = os.pipe()
                children = []
                original_stage, original_copy = Host.stage, wyoming_host.shutil.copy2
                lock = threading.Lock()

                class Rendezvous:
                    def __enter__(guard):
                        if threading.current_thread().name == 'sibling-start':
                            second_attempt.set()
                        self.assertTrue(lock.acquire(timeout=5))

                    def __exit__(guard, *exc):
                        lock.release()

                def copy(source, destination):
                    result = original_copy(source, destination)
                    if threading.current_thread().name == 'first-start':
                        with open(destination, 'ab'):
                            writer_open.set()
                            self.assertTrue(second_attempt.wait(5))
                    return result

                def stage(host):
                    if host is sibling:
                        pid = os.fork()
                        if pid == 0:
                            os.close(release_write)
                            os.read(release_read, 1)
                            os._exit(0)
                        children.append(pid)
                        second_attempt.set()
                    return original_stage(host)

                def start(host, name):
                    threading.current_thread().name = name
                    host.start()
                    self.describe(host)

                try:
                    with mock.patch.object(wyoming_host, 'LAUNCH_GUARD', Rendezvous(), create=True), \
                            mock.patch.object(Host, 'stage', stage), \
                            mock.patch('wyoming_host.shutil.copy2', side_effect=copy), \
                            concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                        a = pool.submit(start, first, 'first-start')
                        self.assertTrue(writer_open.wait(5))
                        b = pool.submit(start, sibling, 'sibling-start')
                        failures = []
                        for future in (a, b):
                            try:
                                future.result(timeout=20)
                            except Exception as error:
                                failures.append(error)
                        self.assertEqual(failures, [], [str(error) for error in failures])
                    self.assertEqual(len(children), 1)
                    self.assertIsNone(sibling.proc.poll())
                    first.stop()
                    self.describe(sibling)
                finally:
                    os.close(release_write)
                    os.close(release_read)
                    for pid in children:
                        self.assertEqual(os.waitpid(pid, 0)[1], 0)

    def test_owned_exit_seven_and_live_stderr_eof_collect_without_probing(self):
        real_launch = subprocess.Popen
        for kind in KINDS:
            for mode in ('exit', 'eof'):
                with self.subTest(kind=kind.__name__, mode=mode):
                    host = self.host(kind)
                    processes, staged = [], []

                    def launch(args, **kwargs):
                        if args[-1] == 'path':
                            return real_launch(args, **kwargs)
                        staged.append(Path(args[0]).parent)
                        script = ('import os; os.write(2, b"synthetic startup rejected\\n"); '
                                  'os._exit(7)' if mode == 'exit' else
                                  'import os; os.close(2); os.read(0, 1)')
                        kwargs['stdin'] = subprocess.PIPE
                        proc = real_launch([sys.executable, '-c', script], **kwargs)
                        processes.append(proc)
                        if mode == 'exit':
                            proc.wait(timeout=2)  # Make status-before-cleanup deterministic.
                        return proc

                    with mock.patch('wyoming_host.subprocess.Popen', side_effect=launch), \
                            mock.patch('socket.create_connection') as probe:
                        with self.assertRaisesRegex(RuntimeError, 'host readiness failed') as error:
                            host.start()
                    detail = str(error.exception)
                    if mode == 'exit':
                        self.assertIn('reason=child-exited status=7', detail)
                        self.assertIn('synthetic startup rejected', detail)
                    else:
                        self.assertIn('reason=stderr-eof status=None', detail)
                    self.assertIsNone(host.proc)
                    self.assertEqual(len(processes), 1)
                    self.assertTrue(processes[0].stderr.closed)
                    processes[0].stdin.close()
                    self.assertEqual(processes[0].returncode,
                                     7 if mode == 'exit' else -signal.SIGKILL)
                    self.assertFalse(staged[0].exists())
                    probe.assert_not_called()

    def test_controller_exit_seven_is_nonzero_without_port_or_cleaned(self):
        runner = r'''
import sys
from pathlib import Path
from unittest import mock
sys.path.insert(0, sys.argv[1])
import test_host_startup as startup
import wyoming_host, bridge_host
controller = wyoming_host if sys.argv[2] == 'base' else bridge_host
kind = startup.Host if sys.argv[2] == 'base' else startup.ObservedHost
original = wyoming_host.subprocess.Popen
children, stages = [], []
def launch(args, **kwargs):
    if args[-1] == 'path':
        return original(args, **kwargs)
    stages.append(Path(args[0]).parent)
    proc = original([sys.executable, '-c',
        'import os; os.write(2, b"synthetic startup rejected\\n"); os._exit(7)'], **kwargs)
    children.append(proc)
    proc.wait(timeout=2)
    return proc
try:
    with mock.patch.object(controller, 'Host', kind), mock.patch.object(
            wyoming_host.subprocess, 'Popen', side_effect=launch):
        controller.main()
finally:
    assert len(children) == 1 and children[0].returncode == 7
    assert children[0].stderr.closed and not stages[0].exists()
    print('owned-resources-collected', file=sys.stderr)
'''
        for kind in ('base', 'observed'):
            with self.subTest(kind=kind):
                result = subprocess.run(
                    [sys.executable, '-c', runner, str(ROOT / 'tests'), kind],
                    input='', capture_output=True, text=True, timeout=25)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(result.stdout, '')
                self.assertIn('reason=child-exited status=7', result.stderr)
                self.assertIn('startup_stderr=synthetic startup rejected', result.stderr)
                self.assertIn('owned-resources-collected', result.stderr)


if __name__ == '__main__':
    unittest.main()
