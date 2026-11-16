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
