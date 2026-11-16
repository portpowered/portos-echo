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
