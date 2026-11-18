"""Offline measurement state-machine and diagnostic regressions (T1)."""
import io, select, socket, sys, tempfile, time, unittest
from pathlib import Path
from unittest import mock

class LogicTests(unittest.TestCase):
    def test_semantic_pause_boundary_and_all_receive_paths(self):
        mic = {"type": "audio-chunk", "data": {"rate": 16000, "width": 2, "channels": 1, "timestamp": 42}}
        pong = ({"type": "pong", "data": {}}, b"")
        windows = []
        for queued in (False, True):
            session = M.Session("unused")
            session.sock, session.proc = mock.Mock(), mock.Mock()
            session.remaining = lambda: 1
            session.mic, session.context = "active", "pause-drain"
            trial = M.Trial(session, None)
            # Existing active witnesses remain; only queued duplicates are omitted.
            for _ in range(2):
                trial.phase()
                ticks = [0, 0, 2] if queued else [0, 0, 0, 0, 2]
                with mock.patch.object(M.C, "read_event", return_value=(mic, bytes(1600))), mock.patch.object(time, "monotonic", side_effect=ticks), mock.patch.object(select, "select", return_value=([session.sock], [], [])):
                    trial.pump(1)
            session.send("pause-satellite")
            events = ([(mic, bytes(1600))] if queued else []) + [({"type": "played", "data": {}}, b""), pong]
            with mock.patch.object(M.C, "read_event", side_effect=events):
                session.acknowledge_pause()
            with mock.patch.object(M.C, "read_event", return_value=pong):
                session.end_run()
            windows.append(M.rows_bytes(session.rows))
        self.assertEqual(*windows)
        fixture = [M.C.json.loads(line) for line in (M.OUTPUT / "barge-in-pause-satellite.jsonl").read_bytes().splitlines()]
        cut = next(i for i, row in enumerate(fixture) if row["header"]["type"] == "pause-satellite")
        M.assert_same(windows[0], M.rows_bytes(fixture[cut - 2:cut + 3]), "controlled pause window")
        print("pause boundary: queued=0/1 canonical=" + windows[0].decode().strip())
        mutations = [(mic, bytes(1601)), (mic, bytes(100) + b"x" + bytes(1499))]
        for key, value in (("rate", 8000), ("width", 1), ("channels", 2), ("timestamp", "42"), ("extra", 0)):
            mutations.append(({**mic, "data": {**mic["data"], key: value}}, bytes(1600)))
        for context in ("active", "barrier", "pause-drain", "cleanup", "recovery"):
            for event in mutations:
                session.mic, session.context = "active", context
                with mock.patch.object(M.C, "read_event", return_value=event), self.assertRaisesRegex(AssertionError, context + ": audio-chunk: expected") as error:
                    session.until("pong", keep=False)
        for kind in ("audio-chunk", "audio-start", "run-pipeline", "played", "unknown"):
            session.mic, session.context = "paused", "post-cut"
            event = (mic, bytes(1600)) if kind == "audio-chunk" else ({"type": kind, "data": {}}, b"")
            with mock.patch.object(M.C, "read_event", return_value=event), self.assertRaisesRegex(AssertionError, "post-cut: " + kind + ": expected") as error:
                session.receive()
        session.stop_pending = True
        with mock.patch.object(M.C, "read_event", return_value=({"type": "played", "data": {}}, b"")):
            session.until("played")
            with self.assertRaisesRegex(AssertionError, "before played"):
                session.until("played")
    def test_end_run_requires_exactly_one_played_only_for_active_reply(self):
        played = ({"type": "played", "data": {}}, b"")
        pong = ({"type": "pong", "data": {}}, b"")
        for active in (False, True):
            session = M.Session("unused")
            session.sock, session.remaining = mock.Mock(), lambda: 1
            session.mic = "active"
            with mock.patch.object(M.C, "read_event", side_effect=([played] if active else []) + [pong]):
                session.end_run(reply_active=active)
            self.assertEqual((session.mic, session.stop_pending), ("idle", False))
            self.assertEqual([row["header"]["type"] for row in session.rows],
                             ["hassmic-end"] + (["played"] if active else []))
            session.mic = "active"
            with mock.patch.object(M.C, "read_event", side_effect=[played, played] if active else [played]), self.assertRaisesRegex(AssertionError, "expected pong|before played"):
                session.end_run(reply_active=active)
        with mock.patch.object(M.C, "read_event", return_value=pong), self.assertRaisesRegex(AssertionError, "expected played, actual=pong"):
            session.end_run(reply_active=True)
    def test_lead_in_is_exact_and_precedes_regular_pcm(self):
        session = M.Session("unused")
        session.sock, session.remaining = mock.Mock(), lambda: 1
        session.mic, session.lead_remaining = "lead", 25600
        event = {"type": "audio-chunk", "data": {"rate": 16000, "width": 2, "channels": 1, "timestamp": 0}}
        for index, size in enumerate([2048] * 12 + [1024, 1600]):
            if index == 1:
                session.send("run-satellite", keep=False)
            with mock.patch.object(M.C, "read_event", return_value=(event, bytes(size))):
                session.receive("audio-chunk")
        self.assertEqual((session.mic, session.lead_remaining), ("active", 0))
        for data, payload in (({"extra": 1}, b""), ({}, b"x")):
            with mock.patch.object(M.C, "read_event", return_value=({"type": "pong", "data": data}, payload)), self.assertRaisesRegex(AssertionError, "expected data=.*payload=0"):
                session.until("pong")
        with mock.patch.object(M.C, "read_event", return_value=(event, bytes(2048))), self.assertRaisesRegex(AssertionError, "expected bytes=1600"):
            session.receive()
    def test_exact_playback_oracle(self):
        for strategy in M.STRATEGIES:
            drained = strategy in ("audio-stop", "none")
            count, played = (144000, 1) if drained else (48000, 1 if strategy == "pause-satellite" else 0)
            playback = mock.Mock(data=(M.PAYLOAD * 30)[:count], end=3)
            trial = mock.Mock(played=played, pipelines=0)
            result = {"bytes": count, "cut_bytes": 45056}
            M.assert_result(strategy, playback, trial, result)
            for field, value, diagnostic in (("played", 2, "bytes/played/EOF"), ("pipelines", 1, "pipelines=0")):
                with mock.patch.object(trial, field, value), self.assertRaisesRegex(AssertionError, diagnostic):
                    M.assert_result(strategy, playback, trial, result)
            for field, value, diagnostic in (("end", None, "bytes/played/EOF"), ("data", b"x" * count, "reply PCM")):
                with mock.patch.object(playback, field, value), self.assertRaisesRegex(AssertionError, diagnostic):
                    M.assert_result(strategy, playback, trial, result)
            with self.assertRaisesRegex(AssertionError, "bytes/played/EOF|bounded abort PCM"):
                M.assert_result(strategy, playback, trial, {"bytes": count - 1 if drained else 143360, "cut_bytes": 45056})
    def test_mismatch_names_the_differing_lines(self):
        M.assert_same(b"a\nb\n", b"a\nb\n", "same")
        with self.assertRaises(AssertionError) as caught:
            M.assert_same(b"a\nX\n", b"a\nb\n", "pause-satellite: fixture mismatch")
        text = str(caught.exception)
        self.assertTrue(text.startswith("pause-satellite: fixture mismatch\n"), text)
        self.assertIn("-b", text.splitlines())
        self.assertIn("+X", text.splitlines())
    def test_measurement_arithmetic(self):
        self.assertEqual(M.elapsed(2, 5), 3000)
        self.assertEqual(M.elapsed(2, None, lambda: 3.5), 1500)
        self.assertEqual(M.summary([4, 1, 2, 3]), (2.5, 4))
    def test_disconnect_closes_both_owners(self):
        session = mock.Mock(); trial = M.Trial(session, None)
        sock, stream = session.sock, session.stream
        trial.disconnect()
        sock.shutdown.assert_called_once_with(socket.SHUT_RDWR)
        sock.close.assert_called_once_with(); stream.close.assert_called_once_with()
        self.assertIsNone(trial.s.sock)
        session.logged.assert_called_once_with("client disconnected")
    def test_envelope_matches_real_digest_and_normalization(self):
        row = M.C.record("bridge->echo", {"type": "audio-chunk", "data": M.C.FORMAT}, M.PAYLOAD)
        self.assertEqual(row["header"]["payload_length"], 4800)
        self.assertEqual(row["payload_sha256"], M.C.hashlib.sha256(M.PAYLOAD).hexdigest())
        self.assertEqual(M.C.normalize({"timestamp": 94, "version": "x"}), {"timestamp": 0, "version": "<version>"})
    def test_failure_and_competition_preserve_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"; output.mkdir()
            (output / "original").write_bytes(b"unchanged")
            for error in (TimeoutError, EOFError, InterruptedError, KeyboardInterrupt):
                def fail(stage, failure=error):
                    (stage / "partial").write_bytes(b"partial")
                    raise failure("controlled failure")
                with self.assertRaises(error):
                    M.publish(output, fail)
                self.assertEqual({p.name: p.read_bytes() for p in output.iterdir()}, {"original": b"unchanged"})
                self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ["fixtures"])
            with M.C.publication(output):
                with self.assertRaises(FileExistsError):
                    M.publish(output, lambda _: None)
            M.publish(output, lambda stage: (stage / "new").write_bytes(b"complete"))
            self.assertEqual((output / "original").read_bytes(), b"unchanged")
            self.assertEqual((output / "new").read_bytes(), b"complete")
            rename = Path.rename
            def failed_rename(source, target):
                if source.name == "stage":
                    raise OSError("controlled rename failure")
                return rename(source, target)
            with mock.patch.object(Path, "rename", failed_rename), self.assertRaises(OSError):
                M.publish(output, lambda stage: (stage / "partial").write_bytes(b"partial"))
            self.assertEqual(sorted(p.name for p in output.iterdir()), ["new", "original"])
            self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ["fixtures"])
class DiagnosticTests(unittest.TestCase):
    def entry(self, action):
        output = io.StringIO()
        with mock.patch.object(M, "main", side_effect=action), mock.patch.object(M.signal, "signal"), mock.patch.object(sys, "stderr", output):
            self.assertEqual(M.entry(), 1)
        return output.getvalue()

    def test_empty_assertion_reports_pre_cleanup_state_and_both_cleanup_errors(self):
        session = M.Session("unused")
        session.proc = mock.Mock(); session.proc.poll.return_value = None
        session.wake = session.send = session.barrier = mock.Mock()
        session.mic, session.lead_remaining = "active", 0
        playback = mock.Mock(data=bytearray(b"1234"), start=0, end=None)
        playback.condition = mock.MagicMock()
        playback.close.side_effect = OSError("FIFO close failed")
        original = AssertionError()
        def fail(_):
            session.context = "post-cut"
            raise original
        with mock.patch.object(M, "Playback", return_value=playback), mock.patch.object(M, "Session", return_value=session), mock.patch.object(type(session), "__enter__", return_value=session), mock.patch.object(M.C.Session, "__exit__", side_effect=OSError("host cleanup failed")), mock.patch.object(M.Trial, "pump", side_effect=fail), mock.patch.object(M, "STRATEGIES", ("none",)):
            text = self.entry(lambda: M.measure(repetitions=1))
        self.assertIn("AssertionError: assertion failed without message", text)
        self.assertIn("strategy=none repetition=1 phase=post-cut pcm_bytes=4 played=0 pipelines=0 playback_started=true playback_eof=false host=alive mic=active lead_remaining=0", text)
        self.assertIn("cleanup: OSError: host cleanup failed", text)
        self.assertIn("cleanup: OSError: FIFO close failed", text)
        self.assertEqual(original.trial_state["phase"], "post-cut")
        playback.close.assert_called_once_with()

    def test_startup_failure_reports_unknown_without_fabricating_trial(self):
        with mock.patch.object(M, "Playback", side_effect=OSError("FIFO creation failed")), mock.patch.object(M, "STRATEGIES", ("none",)):
            text = self.entry(lambda: M.measure(repetitions=1))
        self.assertIn("OSError: FIFO creation failed", text)
        self.assertIn("strategy=none repetition=1 phase=trial-startup pcm_bytes=unknown played=unknown pipelines=unknown playback_started=unknown playback_eof=unknown host=unknown", text)

    def test_fixture_mismatch_retains_available_completed_trial_values(self):
        result = {"bytes": 144000, "played": 1, "pipelines": 0, "end_ms": 20}
        with mock.patch.object(M, "run_trial", return_value=([], result)), mock.patch.object(M, "OUTPUT") as output, mock.patch.object(M, "STRATEGIES", ("none",)):
            (output / "barge-in-none.jsonl").read_bytes.return_value = b"expected\n"
            text = self.entry(lambda: M.measure(repetitions=1))
        self.assertIn("none: fixture mismatch", text)
        self.assertIn("strategy=none repetition=1 phase=fixture-comparison pcm_bytes=144000 played=1 pipelines=0 playback_started=true playback_eof=true host=unknown", text)

    def test_next_trial_does_not_inherit_previous_state(self):
        result = {"bytes": 144000, "played": 1, "pipelines": 0, "end_ms": 20}
        with mock.patch.object(M, "run_trial", side_effect=[([], result), RuntimeError("next startup")]), mock.patch.object(M, "OUTPUT") as output, mock.patch.object(M, "STRATEGIES", ("none",)), mock.patch.object(sys, "stdout", io.StringIO()):
            (output / "barge-in-none.jsonl").read_bytes.return_value = b""
            text = self.entry(lambda: M.measure(repetitions=2))
        self.assertIn("strategy=none repetition=2 phase=trial-startup pcm_bytes=unknown played=unknown", text)
        self.assertIn("RuntimeError: next startup", text)

    def test_publication_failure_reports_phase_and_preserves_originals(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "fixtures"; output.mkdir()
            (output / "prior").write_bytes(b"published")
            rename = Path.rename
            def fail(source, target):
                if source.name == "stage":
                    raise OSError("publication rename failed")
                return rename(source, target)
            with mock.patch.object(Path, "rename", fail):
                text = self.entry(lambda: M.publish(output, lambda stage: (stage / "new").write_bytes(b"new")))
            self.assertIn("OSError: publication rename failed", text)
            self.assertIn("strategy=unknown repetition=unknown phase=publication pcm_bytes=unknown", text)
            self.assertEqual({p.name: p.read_bytes() for p in output.iterdir()}, {"prior": b"published"})
            self.assertEqual([p.name for p in Path(folder).iterdir()], ["fixtures"])

    def test_cleanup_only_failure_is_reported_and_original_host_state_is_retained(self):
        session = M.Session("unused"); session.proc = mock.Mock()
        session.proc.poll.return_value = 7
        session.context = "recovery/wake"
        with mock.patch.object(M.C.Session, "__exit__", side_effect=OSError("cleanup only")):
            text = self.entry(lambda: session.__exit__(None, None, None))
        self.assertIn("OSError: cleanup only", text)
        self.assertIn("phase=cleanup", text)
        self.assertIn("host=exited(7)", text)
        original = EOFError("original wire failure")
        M.retain(original, session=session)
        session.proc.poll.return_value = 0
        M.retain(original, session=session)
        self.assertEqual(original.trial_state["host"], "exited(7)")

    def test_unexpected_exception_still_has_reason_and_unknown_state(self):
        def fail():
            raise KeyError("controlled missing field")
        text = self.entry(fail)
        self.assertIn("KeyError: 'controlled missing field'", text)
        self.assertIn("strategy=unknown repetition=unknown phase=unknown", text)

def run(measurement):
    global M
    M = measurement
    suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(case)
                               for case in (LogicTests, DiagnosticTests))
    return 0 if unittest.TextTestRunner().run(suite).wasSuccessful() else 1
