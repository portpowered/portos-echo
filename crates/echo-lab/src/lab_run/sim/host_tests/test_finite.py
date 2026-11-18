"""T1: finite FIFO ownership with direct calls and in-memory I/O."""
import unittest
import math
import struct
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

    def test_overlap_queues_exact_input_without_another_wake(self):
        pcm = [1, 2] * 800
        self.host.finite_mode = True
        with mock.patch.object(CONTROLLER.signal, "SIGUSR1", 10, create=True), \
                mock.patch.object(CONTROLLER, "emit") as emit:
            with self.assertRaisesRegex(ValueError, "existing finite run"):
                self.host.command({"action": "inject", "pcm": pcm})
            self.host.command({"action": "finite", "pcm": pcm})
            with self.assertRaisesRegex(ValueError, "not drained"):
                self.host.command({"action": "inject", "pcm": pcm})
            self.host.pending = b""
            emit.reset_mock()
            self.host.command({"action": "inject", "pcm": pcm})
            self.assertEqual(self.host.pending, bytes(pcm))
            self.host.proc.send_signal.assert_called_once_with(10)
            emit.assert_called_once_with({"event": "host.ack"})

    def test_playback_reports_exact_sink_counts_without_returning_pcm(self):
        self.host.finite_mode = True
        (self.host.folder / "play.raw").write_bytes(bytes([0, 0, 1, 2]))
        with mock.patch.object(CONTROLLER, "emit") as emit:
            self.host.command({"action": "playback"})
        emit.assert_called_once_with({"event": "host.ack", "pcm_bytes": 4, "nonzero_bytes": 2, "flush": None})
        (self.host.folder / "play.raw").write_bytes(bytes(CONTROLLER.LIMIT + 2))
        with self.assertRaisesRegex(ValueError, "invalid playback observation"):
            self.host.command({"action": "playback"})

    def test_playback_classifies_complete_chime_and_preserves_every_reply_byte(self):
        tones = []
        for frequency in [750, 1000]:
            for index in range(1920):
                gain = (1 - math.cos(math.pi * min(index, 1919 - index, 240) / 240)) / 2
                tones.append(round(983 * gain * math.sin(math.tau * frequency * index / 24000)))
        chime = struct.pack("<3840h", *tones)
        reply = struct.pack("<4800h", *([1000, 0, -1000, 0] * 1200))
        self.host.finite_mode = True
        path = self.host.folder / "play.raw"
        path.write_bytes(chime + reply)
        with mock.patch.object(CONTROLLER, "emit") as emit:
            self.host.command({"action": "playback", "wake_earcon": True})
        emit.assert_called_once_with({"event": "host.ack", "pcm_bytes": 9600,
                                     "nonzero_bytes": 4800, "total_pcm_bytes": 17280,
                                     "earcon_pcm_bytes": 7680,
                                     "earcon_nonzero_bytes": sum(byte != 0 for byte in chime)})
        # No PCM is excluded by default, even if it is quiet or resembles a tone.
        self.assertEqual(CONTROLLER.playback_counts(chime + reply)["pcm_bytes"], 17280)
        extra = CONTROLLER.playback_counts(chime + reply + b"\x01\x02", True)
        self.assertEqual((extra["pcm_bytes"], extra["nonzero_bytes"]), (9602, 4802))
        for data in [chime[:7678], reply, chime[:100] + b"\xff\x7f" + chime[102:] + reply]:
            path.write_bytes(data)
            with mock.patch.object(CONTROLLER, "emit") as emit:
                with self.assertRaisesRegex(ValueError, "host earcon"):
                    self.host.command({"action": "playback", "wake_earcon": True})
                emit.assert_not_called()

    def test_muted_input_needs_native_latch_and_consumption_before_ack(self):
        self.host.finite_mode = self.host.finite = True
        pcm = [1, 2] * 800
        with mock.patch.object(CONTROLLER.signal, "SIGUSR1", 10, create=True), \
                mock.patch.object(CONTROLLER, "emit") as emit:
            with self.assertRaisesRegex(ValueError, "native mute acknowledgment"):
                self.host.command({"action": "muted", "pcm": pcm})
            self.host.proc.send_signal.assert_not_called()
            self.host.muted = True
            original = self.host.fed
            def consumed(predicate, timeout):
                self.assertEqual(timeout, 20)
                self.assertFalse(predicate())
                self.assertEqual(self.host.pending, bytes(pcm) + bytes(8192))
                emit.assert_not_called()
                self.host.fed = original + len(pcm) + 8192
                return predicate()
            with mock.patch.object(self.host.condition, "wait_for", side_effect=consumed):
                self.host.command({"action": "muted", "pcm": pcm})
            emit.assert_called_once_with({"event": "host.ack"})
            self.host.proc.send_signal.assert_called_once_with(10)

    def test_playback_reports_native_flush_counters_and_rejects_ambiguous_evidence(self):
        self.host.finite_mode = True
        (self.host.folder / "play.raw").write_bytes(bytes([1, 0]) * 960)
        record = b"duplex flush: receipt_ms=100 closed_ms=119 before=960 written=1920 discarded=9600\n"
        self.host.log = bytearray(record)
        with mock.patch.object(CONTROLLER, "emit") as emit:
            self.host.command({"action": "playback"})
        emit.assert_called_once_with({"event": "host.ack", "pcm_bytes": 1920, "nonzero_bytes": 960,
                                     "flush": {"receipt_ms": 100, "closed_ms": 119, "before": 960,
                                               "written": 1920, "discarded": 9600}})
        for invalid in [record + record, record + record[:-1], b"duplex flush: malformed\n"]:
            self.host.log = bytearray(invalid)
            with self.assertRaisesRegex(ValueError, "one native flush observation"):
                self.host.command({"action": "playback"})

    def test_post_cut_waits_for_complete_fragmented_log_before_sink_snapshot(self):
        self.host.finite_mode = True
        sink = self.host.folder / "play.raw"
        sink.write_bytes(bytes(960))
        record = b"duplex flush: receipt_ms=100 closed_ms=119 before=960 written=1920 discarded=9600\n"
        with mock.patch.object(CONTROLLER, "emit") as emit:
            def delivery(predicate, timeout):
                self.assertEqual(timeout, 5)
                self.assertFalse(predicate())
                for fragment in [record[:7], record[7:-1]]:
                    self.host.log.extend(fragment)
                    self.assertFalse(predicate())
                    emit.assert_not_called()
                sink.write_bytes(bytes([1, 0]) * 960)
                self.host.log.extend(b"\n")
                return predicate()
            with mock.patch.object(self.host.condition, "wait_for", side_effect=delivery) as wait:
                self.host.command({"action": "playback", "wait_flush": True})
            wait.assert_called_once()
        emit.assert_called_once_with({"event": "host.ack", "pcm_bytes": 1920, "nonzero_bytes": 960,
                                     "flush": {"receipt_ms": 100, "closed_ms": 119, "before": 960,
                                               "written": 1920, "discarded": 9600}})

    def test_pre_cut_queries_do_not_wait_for_missing_or_partial_flush_record(self):
        self.host.finite_mode = True
        (self.host.folder / "play.raw").write_bytes(bytes(960))
        for pending in [b"", b"duplex flush: receipt_ms=100"]:
            self.host.log = bytearray(pending)
            with mock.patch.object(self.host.condition, "wait_for") as wait, \
                    mock.patch.object(CONTROLLER, "emit") as emit:
                self.host.command({"action": "playback"})
            wait.assert_not_called()
            emit.assert_called_once_with({"event": "host.ack", "pcm_bytes": 960,
                                         "nonzero_bytes": 0, "flush": None})

    def test_post_cut_missing_error_cancelled_and_invalid_logs_cannot_ack(self):
        self.host.finite_mode = True
        record = b"duplex flush: receipt_ms=100 closed_ms=119 before=960 written=1920 discarded=9600\n"
        for failure in ["missing", "partial", "error", "cancel", "duplicate", "malformed"]:
            with self.subTest(failure=failure):
                self.host.log = bytearray()
                self.host.error = False
                self.host.cancelled.clear()
                def delivery(predicate, timeout):
                    self.assertEqual(timeout, 5)
                    self.assertFalse(predicate())
                    self.host.log.extend({"partial": record[:-1], "duplicate": record * 2,
                                          "malformed": b"duplex flush: malformed\n"}.get(failure, b""))
                    self.host.error = failure == "error"
                    if failure == "cancel":
                        self.host.cancelled.set()
                    return predicate()
                with mock.patch.object(self.host.condition, "wait_for", side_effect=delivery), \
                        mock.patch.object(CONTROLLER, "emit") as emit:
                    with self.assertRaises((RuntimeError, ValueError)):
                        self.host.command({"action": "playback", "wait_flush": True})
                emit.assert_not_called()
