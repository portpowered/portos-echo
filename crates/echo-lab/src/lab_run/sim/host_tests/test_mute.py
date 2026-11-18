"""Native keypad behavior, with an opt-in T3 against the prebuilt pinned host."""
import json
import os
import socket
import struct
import tempfile
import unittest
from unittest import mock

from test_controls import CONTROLLER


class MuteControls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="lab-mute-unit-")
        self.addCleanup(self.directory.cleanup)
        self.host = CONTROLLER.Host(self.directory.name)
        self.host.keys = 17
        self.host.proc = mock.Mock()

    def test_restart_collects_before_start_and_preserves_cancellation_authority(self):
        events = mock.Mock()
        self.host.proc.pid = 101
        cancelled = self.host.cancelled
        condition = self.host.condition
        with mock.patch.object(self.host, "close") as close, \
                mock.patch.object(self.host, "start") as start, \
                mock.patch.object(CONTROLLER, "emit") as emit:
            events.attach_mock(close, "close")
            events.attach_mock(start, "start")
            events.attach_mock(emit, "ack")
            def started():
                self.assertFalse(self.host.stop.is_set())
                self.assertFalse(self.host.muted)
                self.host.proc.pid = 102
            start.side_effect = started
            self.host.muted = True
            self.host.command({"action": "restart"})
            self.assertEqual(events.mock_calls, [mock.call.close(), mock.call.start(),
                mock.call.ack({"event": "host.ack", "previous_pid": 101, "pid": 102})])
            self.assertIs(self.host.cancelled, cancelled)
            self.assertIs(self.host.condition, condition)
            close.side_effect = RuntimeError("cleanup failed")
            events.reset_mock()
            with self.assertRaises(RuntimeError):
                self.host.command({"action": "restart"})
            self.assertEqual(events.mock_calls, [mock.call.close()])

    def test_key_release_toggles_once_and_acknowledges_after_observed_state(self):
        events = mock.Mock()
        with mock.patch.object(CONTROLLER.os, "write") as write, \
                mock.patch.object(self.host, "logged") as logged, \
                mock.patch.object(CONTROLLER, "emit") as emit:
            events.attach_mock(write, "write")
            events.attach_mock(logged, "observed")
            events.attach_mock(emit, "ack")
            packet = b"".join(struct.pack("@llHHi", 0, 0, 1, 113, v) for v in [1, 0])
            write.return_value = len(packet)
            for muted in [True, False, True]:
                events.reset_mock()
                self.host.command({"action": "mute"})
                self.assertEqual(events.mock_calls, [mock.call.write(17, packet),
                    mock.call.observed(f"mic mute button: {int(muted)}\n".encode()),
                    mock.call.ack({"event": "host.ack"})])
                self.assertEqual(self.host.muted, muted)
            self.host.proc.send_signal.assert_not_called()

    def test_write_or_observation_failure_cannot_acknowledge_or_advance_state(self):
        for failure in ["missing", "short", "write", "observation"]:
            with self.subTest(failure=failure), mock.patch.object(CONTROLLER.os, "write") as write, \
                    mock.patch.object(self.host, "logged") as logged, \
                    mock.patch.object(CONTROLLER, "emit") as emit:
                self.host.keys = None if failure == "missing" else 17
                write.return_value = 0 if failure == "short" else 2 * struct.calcsize("@llHHi")
                if failure == "write":
                    write.side_effect = OSError("synthetic failure")
                if failure == "observation":
                    logged.side_effect = RuntimeError("host readiness failed")
                with self.assertRaises((OSError, RuntimeError)):
                    self.host.command({"action": "mute"})
                emit.assert_not_called()
                self.assertFalse(self.host.muted)

    def test_keypad_descriptor_is_closed_on_cleanup_once(self):
        self.host.proc.poll.return_value = 0
        with mock.patch.object(CONTROLLER.os, "close") as close:
            self.host.close()
            self.host.close()
            close.assert_called_once_with(17)
        self.assertIsNone(self.host.keys)


@unittest.skipUnless(os.name == "posix" and os.environ.get("ECHO_LAB_HOST_MUTE_T3") == "1",
                     "T3 opt-in requires prebuilt pinned hassmic-host on Linux")
class RealHostMute(unittest.TestCase):
    def test_t3_native_mute_transitions_snapshot_and_unmuted_wake(self):
        with tempfile.TemporaryDirectory(prefix="lab-mute-t3-") as directory:
            host = CONTROLLER.Host(directory)
            try:
                with mock.patch.object(CONTROLLER, "emit") as emit:
                    host.start()
                    address = emit.call_args.args[0]["address"]
                name, port = address.split(":")
                with socket.create_connection((name, int(port)), timeout=5) as peer:
                    peer.settimeout(5)
                    with peer.makefile("rb") as stream:
                        peer.sendall(b'{"type":"run-satellite"}\n{"type":"describe"}\n')
                        self.assertEqual(self.event(stream)[0], "info")
                        host.command({"action": "registered"})
                        host.command({"action": "mute"})
                        self.assertEqual(self.event(stream), ("hassmic-mute", {"muted": True}))
                host.logged(b"client disconnected")
                with socket.create_connection((name, int(port)), timeout=5) as peer:
                    peer.settimeout(5)
                    with peer.makefile("rb") as stream:
                        self.assertEqual(self.event(stream), ("hassmic-mute", {"muted": True}))
                        peer.sendall(b'{"type":"run-satellite"}\n{"type":"describe"}\n')
                        self.assertEqual(self.event(stream)[0], "info")
                        host.command({"action": "registered"})
                        host.command({"action": "mute"})
                        self.assertEqual(self.event(stream), ("hassmic-mute", {"muted": False}))
                        host.command({"action": "say", "pcm": [1, 0, 2, 0]})
                        self.assertEqual(self.event(stream)[0], "run-pipeline")
                        for _ in range(10):
                            self.assertEqual(self.event(stream)[0], "audio-chunk")
                print("T3 native KEY_MUTE: transitions=[true,false], reconnect_snapshot=true, wake_mic_frames=10")
            finally:
                host.close()
                self.assertIsNotNone(host.proc.returncode)
                self.assertIsNone(host.keys)

    @staticmethod
    def event(stream):
        header = json.loads(stream.readline())
        data = header.get("data", {})
        size = header.get("data_length", 0)
        if size:
            data = json.loads(stream.read(size))
        payload_size = header.get("payload_length", 0)
        payload = stream.read(payload_size)
        if len(payload) != payload_size:
            raise RuntimeError("incomplete host payload")
        return header["type"], data
