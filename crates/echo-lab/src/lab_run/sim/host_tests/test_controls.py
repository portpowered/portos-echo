"""T1 controller tests: fake process, no sockets, child or real clock."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("sim_host", Path(__file__).resolve().parents[1] / "host.py")
CONTROLLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTROLLER)


class Controls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="lab-host-unit-")
        self.addCleanup(self.directory.cleanup)
        self.host = CONTROLLER.Host(self.directory.name)
        self.host.proc = mock.Mock()

    def test_supported_controls_send_exact_signal_once(self):
        signals = {"SIGUSR1": 10, "SIGUSR2": 12, "SIGWINCH": 28, "SIGHUP": 1}
        with mock.patch.multiple(CONTROLLER.signal, create=True, **signals), mock.patch.object(CONTROLLER, "emit") as emit:
            for action, expected in [("wake", 10), ("action", 12), ("pair", 28), ("stop", 1)]:
                self.host.proc.reset_mock()
                emit.reset_mock()
                self.host.command({"action": action})
                self.host.proc.send_signal.assert_called_once_with(expected)
                emit.assert_called_once_with({"event": "host.ack"})

    def test_registration_waits_for_log_without_sending_a_signal(self):
        with mock.patch.object(self.host, "logged") as logged, mock.patch.object(CONTROLLER, "emit") as emit:
            self.host.command({"action": "registered"})
            logged.assert_called_once_with(b"satellite: running")
            self.host.proc.send_signal.assert_not_called()
            emit.assert_called_once_with({"event": "host.ack"})

    def test_eof_interrupts_readiness_and_rejects_further_commands(self):
        events = CONTROLLER.queue.Queue()
        with mock.patch.object(CONTROLLER, "launcher_input") as reader:
            CONTROLLER.host_input(self.host, events)
            reader.assert_called_once_with(events)
        for marker in [b"wyoming on ", b"satellite: running"]:
            with self.assertRaisesRegex(RuntimeError, "host readiness failed"):
                self.host.logged(marker)
        with self.assertRaisesRegex(RuntimeError, "host command cancelled"):
            self.host.command({"action": "wake"})
        self.host.proc.send_signal.assert_not_called()

    def test_unsupported_controls_and_invalid_pcm_cannot_signal(self):
        # Supply Unix signal names on Windows without starting the host backend.
        with mock.patch.multiple(CONTROLLER.signal, create=True, SIGUSR1=10, SIGUSR2=12, SIGWINCH=28, SIGHUP=1):
            with self.assertRaises(KeyError):
                self.host.command({"action": "mute"})
            for pcm in [[], [1], [0] * 4098]:
                with self.assertRaises(ValueError):
                    self.host.command({"action": "say", "pcm": pcm})
            self.host.proc.send_signal.assert_not_called()


class Cleanup(unittest.TestCase):
    def setUp(self):
        Controls.setUp(self)
        patch = mock.patch.object(CONTROLLER.signal, "SIGKILL", 9, create=True)
        patch.start()
        self.addCleanup(patch.stop)
    def test_graceful_group_signal_precedes_wait_and_is_idempotent(self):
        self.host.proc.poll.return_value = None
        with mock.patch.object(CONTROLLER.os, "killpg", create=True) as signal:
            self.host.close()
            self.host.close()
            signal.assert_called_once_with(self.host.proc.pid, CONTROLLER.signal.SIGTERM)
            self.assertEqual(self.host.proc.wait.call_args_list, [mock.call(timeout=3)])
            self.host.proc.stderr.close.assert_called_once_with()

    def test_timeout_kills_then_collects_before_closing_pipes(self):
        self.host.proc.poll.return_value = None
        self.host.proc.wait.side_effect = [CONTROLLER.subprocess.TimeoutExpired("fixture", 3), 0]
        events = mock.Mock()
        events.attach_mock(self.host.proc.wait, "wait")
        events.attach_mock(self.host.proc.stderr.close, "close")
        with mock.patch.object(CONTROLLER.os, "killpg", create=True) as signal:
            events.attach_mock(signal, "signal")
            self.host.close()
            self.assertEqual(events.mock_calls, [
                mock.call.signal(self.host.proc.pid, CONTROLLER.signal.SIGTERM),
                mock.call.wait(timeout=3),
                mock.call.signal(self.host.proc.pid, CONTROLLER.signal.SIGKILL),
                mock.call.wait(), mock.call.close()])

    @unittest.skipIf(CONTROLLER.os.name == "nt", "T3 Unix process-group boundary")
    def test_t3_real_group_child_status_is_collected(self):
        import subprocess
        import sys
        proc = subprocess.Popen([sys.executable, "-u", "-c",
                                 "import signal,sys; signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)); print('ready',flush=True); sys.stdin.read()"],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                start_new_session=True)
        self.host.proc = proc
        try:
            self.assertEqual(proc.stdout.readline(), b"ready\n")
            self.host.close()
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.wait(), 0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            proc.stdin.close()
            proc.stdout.close()


class LauncherOwnership(unittest.TestCase):
    """T1: deadline and status decisions with in-memory processes and clocks."""
    def test_eof_before_force_and_repeated_collection_never_terminates(self):
        proc = mock.Mock()
        proc.wait.return_value = 7
        owner = CONTROLLER.LauncherChild(proc)
        with mock.patch.object(CONTROLLER.time, "monotonic", return_value=100):
            self.assertEqual(owner.close(), 7)
            self.assertEqual(owner.close(), 7)
        proc.stdin.close.assert_called_once_with()
        proc.wait.assert_called_once_with(timeout=4)
        proc.kill.assert_not_called()

    def test_timeout_forces_once_and_collects_before_return(self):
        proc = mock.Mock()
        proc.wait.side_effect = [CONTROLLER.subprocess.TimeoutExpired("owned", 4), 1]
        events = mock.Mock()
        for name, child in [("eof", proc.stdin.close), ("wait", proc.wait), ("force", proc.kill)]:
            events.attach_mock(child, name)
        owner = CONTROLLER.LauncherChild(proc)
        with mock.patch.object(CONTROLLER.time, "monotonic", side_effect=[100, 101]):
            self.assertEqual(owner.close(), 1)
            self.assertEqual(owner.close(), 1)
        self.assertEqual(events.mock_calls, [mock.call.eof(), mock.call.wait(timeout=3),
                                            mock.call.force(), mock.call.wait()])

    def test_collection_failure_keeps_handle_and_original_deadline(self):
        proc = mock.Mock()
        proc.wait.side_effect = [OSError("synthetic private marker"),
                                CONTROLLER.subprocess.TimeoutExpired("owned", 0), 1]
        owner = CONTROLLER.LauncherChild(proc)
        with mock.patch.object(CONTROLLER.time, "monotonic", side_effect=[100, 101, 104]):
            with self.assertRaisesRegex(CONTROLLER.LauncherError, "cleanup: cannot (force|collect) launcher child"):
                owner.close()
            self.assertIs(owner.proc, proc)
            self.assertIsNone(owner.status)
            self.assertEqual(owner.close(), 1)
        proc.stdin.close.assert_called_once_with()
        self.assertEqual(proc.wait.call_args_list, [mock.call(timeout=3), mock.call(timeout=0), mock.call()])
        proc.kill.assert_called_once_with()

    def test_partial_reader_setup_failure_collects_acquired_child(self):
        proc = mock.Mock()
        proc.wait.return_value = 0
        with mock.patch.object(CONTROLLER.subprocess, "Popen", return_value=proc), \
                mock.patch.object(CONTROLLER.threading, "Thread", side_effect=RuntimeError("synthetic private marker")), \
                mock.patch.object(CONTROLLER, "emit") as emit:
            self.assertEqual(CONTROLLER.launch_windows(), 1)
        proc.stdin.close.assert_called_once_with()
        proc.wait.assert_called_once()
        proc.kill.assert_not_called()
        emit.assert_called_once_with({"event": "host.failed"})

    def test_force_and_collection_errors_cannot_become_success(self):
        for stage in ["force", "collect"]:
            proc = mock.Mock()
            proc.wait.side_effect = CONTROLLER.subprocess.TimeoutExpired("owned", 0)
            if stage == "force":
                proc.kill.side_effect = OSError("synthetic private marker")
            else:
                proc.wait.side_effect = [CONTROLLER.subprocess.TimeoutExpired("owned", 0), OSError("private")]
            owner = CONTROLLER.LauncherChild(proc)
            with self.assertRaisesRegex(CONTROLLER.LauncherError, "cleanup: cannot (force|collect) launcher child"):
                owner.close()
            self.assertIsNone(owner.status)
            self.assertIs(owner.proc, proc)

    def test_observation_and_input_close_failures_stay_sanitized(self):
        proc = mock.Mock()
        proc.poll.side_effect = OSError("synthetic private marker")
        proc.stdin.close.side_effect = OSError("synthetic private marker")
        proc.wait.return_value = 0
        owner = CONTROLLER.LauncherChild(proc)
        with self.assertRaisesRegex(CONTROLLER.LauncherError, "^cleanup: cannot observe launcher child$"):
            owner.exited()
        for _ in range(2):
            with self.assertRaisesRegex(CONTROLLER.LauncherError, "^cleanup: cannot close launcher input$"):
                owner.close()
        self.assertEqual(owner.status, 0)
        proc.wait.assert_called_once()
        proc.kill.assert_not_called()


def real_wsl_launcher_fixture(audit):
    """Real WSL and pinned Host, with a test-owned guest collection observer.

    The guest observer writes only after Host.close has waited the acquired
    process. A missing record cannot be replaced by a reaped Windows client.
    No downloads or builds are performed here.
    """
    import subprocess
    import sys
    popen = subprocess.Popen
    guest = r'''
import json,runpy,subprocess,sys
from pathlib import Path
ns=runpy.run_path(sys.argv[1])
host=ns['Host']
start,close,logged=host.start,host.close,host.logged
observer = r"""
import json,os,select,signal,sys
from pathlib import Path
fd=os.pidfd_open(int(sys.argv[1]))
print('observer.ready',flush=True)
try:
    sys.stdin.buffer.read()
    survivors=int(not bool(select.select([fd],[],[],0)[0]))
    if survivors:
        signal.pidfd_send_signal(fd,signal.SIGTERM)
    cleaned=bool(select.select([fd],[],[],4)[0])
    if not cleaned:
        signal.pidfd_send_signal(fd,signal.SIGKILL)
        cleaned=bool(select.select([fd],[],[],4)[0])
    Path(sys.argv[2]+'.exit').write_text(json.dumps({'owned_survivors':survivors,'fixture_cleaned':cleaned}))
finally:
    os.close(fd)
"""
def observed_start(self):
    start(self)
    self.observer=subprocess.Popen([sys.executable,'-u','-c',observer,str(self.proc.pid),sys.argv[2]],
        stdin=subprocess.PIPE,stdout=subprocess.PIPE,start_new_session=True)
    assert self.observer.stdout.readline()==b'observer.ready\n'
    print(json.dumps({'event':'guest.owned','pid':self.proc.pid}),flush=True)
def observed_logged(self,marker):
    with self.condition:
        if marker==b'satellite: running':
            print(json.dumps({'event':'guest.command.entered'}),flush=True)
        return logged(self,marker)
def observed_close(self):
    close(self)
    self.observer.communicate(timeout=1)
    assert self.observer.returncode==0
    Path(sys.argv[2]).write_text(json.dumps({'pid':self.proc.pid,'status':self.proc.returncode,'closed':self.closed}))
host.start,host.close,host.logged=observed_start,observed_close,observed_logged
sys.exit(ns['main']())
'''
    # Resolve the test-owned observation path with a bounded, collected client.
    path = popen(["wsl", "-d", "Ubuntu", "--exec", "wslpath", "-u", audit],
                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        output, _ = path.communicate(timeout=4)
        if path.returncode != 0:
            raise RuntimeError("test observer path translation failed")
    finally:
        if path.poll() is None:
            path.kill()
        path.wait()
        path.stdout.close()
    guest_audit = output.decode("utf-8").strip()

    def boundary(argv, **kwargs):
        if "python3" in argv:
            argv = argv[:-1] + ["-c", guest, argv[-1], guest_audit]
        return popen(argv, **kwargs)
    CONTROLLER.subprocess.Popen = boundary
    sys.exit(CONTROLLER.launch_windows())


def native_launcher_fixture(mode, port, audit):
    """Substitute only wsl argv; everything else is the delivered launcher."""
    import subprocess
    import sys
    popen = subprocess.Popen
    child = r'''
import json,os,socket,sys,threading
mode,phase,port = sys.argv[1:]
with socket.create_connection(('127.0.0.1',int(port))) as control:
    control.sendall((json.dumps({'phase':phase,'pid':os.getpid()})+'\n').encode())
    assert control.recv(1)==b'G'
if phase=='translation' and mode!='translation-stall':
    if mode=='translation-fail':
        sys.stderr.write('synthetic private marker')
        sys.exit(7)
    print('/controlled/host.py',flush=True)
else:
    if mode.endswith('stall'):
        threading.Event().wait()
    data=sys.stdin.buffer.read()
    if mode=='forward':
        assert data==b'{"action":"wake"}\n'
    if mode=='setup-fail':
        sys.exit(0)
    if mode=='status':
        sys.stderr.write('synthetic private marker')
        sys.exit(7)
    sys.stdout.buffer.write(b'{"event": "host.closed"}\n')
    sys.stdout.buffer.flush()
'''

    def boundary(argv, **kwargs):
        phase = "translation" if "wslpath" in argv else "running"
        if phase == "running" and mode == "spawn-fail":
            raise OSError("synthetic private marker")
        proc = popen([sys.executable, "-u", "-c", child, mode, phase, str(port)], **kwargs)
        kill, wait = proc.kill, proc.wait

        def force():
            with open(audit, "a", encoding="utf-8") as log:
                log.write(phase + " force\n")
            return kill()

        def collect(*args, **options):
            status = wait(*args, **options)
            with open(audit, "a", encoding="utf-8") as log:
                log.write(f"{phase} collected {status}\n")
            return status
        proc.kill, proc.wait = force, collect
        return proc

    CONTROLLER.subprocess.Popen = boundary
    original_thread = CONTROLLER.threading.Thread

    def setup_thread(*args, **kwargs):
        if mode == "setup-fail" and kwargs.get("target") == CONTROLLER.launcher_write:
            raise RuntimeError("synthetic private marker")
        return original_thread(*args, **kwargs)
    CONTROLLER.threading.Thread = setup_thread
    sys.exit(CONTROLLER.launch_windows())


def native_handle_observer(pid):
    """Rust tests retain this OS observer before releasing any fixture child."""
    import ctypes
    from ctypes import wintypes
    import json
    import sys
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x101001, False, int(pid))
    if not handle:
        sys.exit(1)
    try:
        print('{"event":"observer.ready"}', flush=True)
        sys.stdin.buffer.readline()
        result = kernel.WaitForSingleObject(handle, 4000)
        if result != 0:
            raise RuntimeError("owned child survived")
        code = wintypes.DWORD()
        if not kernel.GetExitCodeProcess(handle, ctypes.byref(code)):
            raise RuntimeError("cannot collect status")
        print(json.dumps({"event": "observer.collected", "status": code.value, "owned_survivors": 0}), flush=True)
    finally:
        if kernel.WaitForSingleObject(handle, 0) == 258:
            kernel.TerminateProcess(handle, 1)
            kernel.WaitForSingleObject(handle, 4000)
        kernel.CloseHandle(handle)


@unittest.skipUnless(CONTROLLER.os.name == "nt", "T3 native Windows retained process handles")
class NativeLauncher(unittest.TestCase):
    """T3 controlled wsl boundary; real wrapper, descendant, pipe and OS waits."""
    def setUp(self):
        import ctypes
        from ctypes import wintypes
        import socket
        import subprocess
        import sys
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        self.kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        self.kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.handles = []
        self.directory = tempfile.TemporaryDirectory(prefix="lab-native-owner-")
        self.addCleanup(self.directory.cleanup)
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(10)
        self.addCleanup(self.listener.close)
        self.audit = Path(self.directory.name) / "owned.log"
        self.wrapper = None
        self.addCleanup(self.cleanup_owned)
        self.sentinel = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.buffer.read()"],
                                         stdin=subprocess.PIPE)

    def cleanup_owned(self):
        if self.wrapper:
            if self.wrapper.poll() is None:
                self.wrapper.kill()
            self.wrapper.wait(timeout=5)
            for pipe in [self.wrapper.stdin, self.wrapper.stdout, self.wrapper.stderr]:
                if pipe and not pipe.closed:
                    pipe.close()
        for handle in self.handles:
            if self.kernel.WaitForSingleObject(handle, 0) == 258:
                self.kernel.TerminateProcess(handle, 1)
                self.assertEqual(self.kernel.WaitForSingleObject(handle, 5000), 0)
            self.kernel.CloseHandle(handle)
        self.sentinel.stdin.close()
        self.sentinel.wait(timeout=5)

    def ready(self, phase):
        import ctypes
        import json
        with self.listener.accept()[0] as control:
            control.settimeout(10)
            with control.makefile("rb") as stream:
                value = json.loads(stream.readline())
            self.assertEqual(value["phase"], phase)
            handle = self.kernel.OpenProcess(0x101001, False, value["pid"])
            self.assertTrue(handle, ctypes.get_last_error())
            self.handles.append(handle)
            control.sendall(b"G")
        return handle

    def run_case(self, mode):
        import ctypes
        from ctypes import wintypes
        import subprocess
        import sys
        source = ("import sys; sys.path.insert(0,sys.argv[1]); from test_controls import native_launcher_fixture; "
                  "native_launcher_fixture(sys.argv[2],int(sys.argv[3]),sys.argv[4])")
        self.wrapper = subprocess.Popen(
            [sys.executable, "-u", "-c", source, str(Path(__file__).parent), mode,
             str(self.listener.getsockname()[1]), str(self.audit)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.ready("translation")
        if mode not in ["translation-stall", "translation-fail", "spawn-fail"]:
            self.ready("running")
        if mode == "forward":
            self.wrapper.stdin.write(b'{"action":"wake"}\n')
            self.wrapper.stdin.flush()
        elif mode == "blocked-write-stall":
            self.wrapper.stdin.write(b"x" * (CONTROLLER.LIMIT - 2) + b"\n")
            self.wrapper.stdin.flush()
        if mode not in ["translation-fail", "spawn-fail"]:
            self.wrapper.stdin.close()
        successful = mode in ["normal", "forward"]
        self.assertEqual(self.wrapper.wait(timeout=10), 0 if successful else (7 if mode == "status" else 1))
        output = self.wrapper.stdout.read()
        error = self.wrapper.stderr.read()
        self.assertEqual(output, b'{"event": "host.closed"}\n' if successful else (
            b'{"event": "host.failed"}\r\n' if mode in ["translation-fail", "spawn-fail", "setup-fail"] else b""))
        self.assertNotIn(b"synthetic private marker", output + error)
        statuses = []
        for handle in self.handles:
            self.assertEqual(self.kernel.WaitForSingleObject(handle, 0), 0)
            code = wintypes.DWORD()
            self.assertTrue(self.kernel.GetExitCodeProcess(handle, ctypes.byref(code)))
            statuses.append(code.value)
        expected = [1] if mode == "translation-stall" else ([7] if mode == "translation-fail" else [0])
        if mode not in ["translation-stall", "translation-fail", "spawn-fail"]:
            expected.append(7 if mode == "status" else int(mode.endswith("stall")))
        self.assertEqual(statuses, expected)
        self.assertIsNone(self.sentinel.poll())
        audit = self.audit.read_text()
        self.assertEqual(audit.count(" force\n"), int(mode.endswith("stall")))
        for phase, status in zip(["translation", "running"], statuses):
            self.assertIn(f"{phase} collected {status}\n", audit)
        print(f"N-WIN {mode}: wrapper_status={self.wrapper.returncode}, descendant_statuses={statuses}, "
              f"owned_survivors=0, force_requests={audit.count(' force' + chr(10))}, sentinel_alive=true, "
              f"stdout={output!r}")

    def test_t3_normal_eof_collects_exact_marker(self):
        self.run_case("normal")

    def test_t3_commands_are_forwarded_exactly_before_eof(self):
        self.run_case("forward")

    def test_t3_blocked_pipe_writer_cannot_block_owned_cleanup(self):
        self.run_case("blocked-write-stall")

    def test_t3_translation_stall_is_forced_and_collected(self):
        self.run_case("translation-stall")

    def test_t3_running_stall_is_forced_and_collected(self):
        self.run_case("running-stall")

    def test_t3_nonzero_status_is_preserved(self):
        self.run_case("status")

    def test_t3_translation_failure_is_sanitized_and_collected(self):
        self.run_case("translation-fail")

    def test_t3_partial_spawn_failure_keeps_translation_collected(self):
        self.run_case("spawn-fail")

    def test_t3_partial_writer_setup_failure_collects_running_child(self):
        self.run_case("setup-fail")
