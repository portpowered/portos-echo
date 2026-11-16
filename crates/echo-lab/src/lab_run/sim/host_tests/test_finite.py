"""T1: finite FIFO ownership with direct calls and in-memory I/O."""
import unittest
from unittest import mock

import test_controls as controls

CONTROLLER = controls.CONTROLLER


class FiniteControls(unittest.TestCase):
    setUp = controls.Controls.setUp

    def test_finite_is_explicit_once_and_queues_exact_bytes(self):
        pcm = [1, 2] * 800
        with mock.patch.object(CONTROLLER.signal, "SIGUSR1", 10, create=True), \
                mock.patch.object(CONTROLLER, "emit") as emit:
            with self.assertRaisesRegex(ValueError, "invalid finite fixture"):
                self.host.command({"action": "finite", "pcm": pcm})
            self.host.proc.send_signal.assert_not_called()
            self.host.finite_mode = True
            self.host.command({"action": "finite", "pcm": pcm})
            self.assertEqual(self.host.pending, bytes(pcm))
            self.host.proc.send_signal.assert_called_once_with(10)
            emit.assert_called_once_with({"event": "host.ack"})
            with self.assertRaisesRegex(ValueError, "already sent"):
                self.host.command({"action": "finite", "pcm": pcm})
            self.host.proc.send_signal.assert_called_once_with(10)
            for invalid in [[], [0] * 1598, [0] * 4800]:
                with self.assertRaisesRegex(ValueError, "invalid finite fixture"):
                    self.host.command({"action": "finite", "pcm": invalid})

    def test_partial_fifo_write_retains_exact_unsent_tail(self):
        self.host.finite_mode = True
        self.host.fd = 99
        self.host.pending = bytes([1, 2, 3, 4])
        def write(fd, data):
            self.assertEqual(fd, 99)
            if data == bytes([3, 4]):
                self.host.stop.set()
            return 2
        with mock.patch.object(CONTROLLER.select, "select", return_value=([], [99], [])), \
                mock.patch.object(CONTROLLER.os, "write", side_effect=write) as output:
            self.host.feed()
        self.assertEqual(output.call_args_list, [mock.call(99, bytes([1, 2, 3, 4])), mock.call(99, bytes([3, 4]))])
        self.assertEqual(self.host.pending, b"")
        self.assertEqual(self.host.fed, 4)

    def test_playback_reports_exact_sink_counts_without_returning_pcm(self):
        self.host.finite_mode = True
        (self.host.folder / "play.raw").write_bytes(bytes([0, 0, 1, 2]))
        with mock.patch.object(CONTROLLER, "emit") as emit:
            self.host.command({"action": "playback"})
        emit.assert_called_once_with({"event": "host.ack", "pcm_bytes": 4, "nonzero_bytes": 2})
        (self.host.folder / "play.raw").write_bytes(bytes(CONTROLLER.LIMIT + 2))
        with self.assertRaisesRegex(ValueError, "invalid playback observation"):
            self.host.command({"action": "playback"})


