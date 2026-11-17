#!/usr/bin/env python3
"""Measure pinned -D host cancellation on loopback; never contact a device.
Ten trials per strategy; --self-test runs one and compares bounded wire windows.
Playback FIFO EOF is an independent close endpoint. An open sink at the bounded
observation deadline is censored, never a fabricated playback-end timestamp.
"""
import argparse, difflib, importlib.util, os, select, shutil, signal, socket
import statistics, subprocess, sys, tempfile, threading, time, unittest
from pathlib import Path
from unittest import mock
SPEC = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("capture-wyoming.py"))
C = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(C)
STRATEGIES = ("audio-stop", "pause-satellite", "close-stream", "none")
OUTPUT = C.ROOT / "tests/fixtures/wyoming"
PAYLOAD = b"\x01\x00" * 2400
def elapsed(start, end, clock=time.monotonic):
    return 1000 * ((clock() if end is None else end) - start)
def summary(samples):
    return statistics.median(samples), max(samples)
class Playback:
    """Observe bytes and writer EOF, independently of the Wyoming connection."""
    def __init__(self, folder):
        self.path = Path(folder) / "play.raw"
        os.mkfifo(self.path)
        self.fd = os.open(self.path, os.O_RDONLY | os.O_NONBLOCK)
        self.anchor = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
        self.data = bytearray()
        self.start = self.end = None
        self.stop = threading.Event()
        self.condition = threading.Condition()
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()
    def read(self):
        while not self.stop.is_set():
            if not select.select([self.fd], [], [], 0.05)[0]:
                continue
            part = os.read(self.fd, 65536)
            with self.condition:
                if part:
                    if self.start is None:
                        self.start = time.monotonic()
                        os.close(self.anchor)
                        self.anchor = None
                    self.data.extend(part)
                    self.condition.notify_all()
                elif self.start is not None:
                    self.end = time.monotonic()
                    self.condition.notify_all()
                    return
    def close(self):
        self.stop.set()
        self.thread.join(timeout=1)
        os.close(self.fd)
        if self.anchor is not None:
            os.close(self.anchor)
class Session(C.Session):
    """S2 reads validate before projecting repeated microphone samples."""
    mic = "idle"
    stop_pending = False
    context = "startup"
    def send(self, kind, data=None, payload=b"", keep=True):
        if kind == "audio-stop":
            self.stop_pending = True
        # A resume on this socket cannot discard lead-in already queued by the host.
        if kind == "run-satellite" and self.mic != "lead":
            self.mic = "active"
        super().send(kind, data, payload, keep)
    def receive(self, expected=None):
        self.sock.settimeout(self.remaining())
        event, payload = C.read_event(self.stream)
        kind, data = event["type"], event["data"]
        label = f"{self.context}: {kind}"
        if kind == "audio-chunk":
            assert self.mic in ("active", "lead"), f"{label}: expected no microphone while {self.mic}"
            want = {"rate": 16000, "width": 2, "channels": 1, "timestamp": 0}
            assert type(data.get("timestamp")) is int and data["timestamp"] >= 0, f"{label}: expected nonnegative integer timestamp, actual={data}"
            assert C.normalize(data) == want and all(type(data[k]) is int for k in want), f"{label}: expected format={want}, actual={data}"
            size = min(2048, self.lead_remaining) if self.mic == "lead" else 1600
            assert payload == bytes(size), (f"{label}: expected bytes={size} sha256={C.hashlib.sha256(bytes(size)).hexdigest()}, "
                                            f"actual bytes={len(payload)} sha256={C.hashlib.sha256(payload).hexdigest()}")
            if self.mic == "lead":
                self.lead_remaining -= size
            self.mic = "lead" if self.mic == "lead" and self.lead_remaining else "active"
        else:
            assert self.mic != "lead", f"{label}: expected remaining lead-in bytes={self.lead_remaining}"
            assert kind == expected, f"{label}: expected {expected or 'audio-chunk'}, actual={kind}"
            want = ({"start_stage": "asr", "end_stage": "tts", "restart_on_end": False, "wake_word_name": "alexa"}
                    if kind == "run-pipeline" else data if kind == "info" else {})
            assert data == want and not payload, f"{label}: expected data={want} payload=0, actual data={data} payload={len(payload)}"
            if kind == "played":
                assert self.stop_pending, f"{label}: expected audio-stop before played"
                self.stop_pending = False
            if kind == "run-pipeline":
                self.mic, self.lead_remaining = "lead", 25600
        return event, payload
    def until(self, kind, keep=True, forbid=()):
        while True:
            event, payload = self.receive(kind)
            assert event["type"] not in forbid, f"{self.context}: forbidden {event['type']} while awaiting {kind}"
            if event["type"] == kind:
                row = C.record("echo->bridge", event, payload)
                if keep:
                    self.rows.append(row)
                return row
    def acknowledge_pause(self):
        self.context = "pause-drain"
        self.barrier(forbid=("played", "run-pipeline"))
        self.mic = "paused"
    def wake(self):
        self.context = "recovery/wake"
        self.send("run-satellite")
        self.barrier()
        self.proc.send_signal(signal.SIGUSR1)
        self.until("run-pipeline")
        event, payload = self.receive("audio-chunk")
        assert event["type"] == "audio-chunk", "wake: expected direct lead-in audio-chunk"
        self.rows.append(C.record("echo->bridge", event, payload))
class Trial:
    def __init__(self, session, playback):
        self.s, self.p = session, playback
        self.chunk, self.played, self.pipelines = False, 0, 0
    def phase(self):
        self.chunk = False
    def pump(self, end):
        while time.monotonic() < end:
            self.s.remaining()
            ready = select.select([self.s.sock, self.s.proc.stderr], [], [],
                                  max(0, min(0.05, end - time.monotonic())))[0]
            if self.s.proc.stderr in ready:
                self.s.logs()
            if self.s.sock in ready:
                event, payload = self.s.receive("played" if self.s.stop_pending else None)
                kind = event["type"]
                if kind == "audio-chunk":
                    if self.chunk:
                        continue
                    self.chunk = True
                elif kind == "played":
                    self.played += 1
                self.s.rows.append(C.record("echo->bridge", event, payload))
    def wake_speaking(self):
        self.s.logs()
        offset = len(self.s.log)
        self.s.proc.send_signal(signal.SIGUSR1)
        self.s.logged("left to the client (-D)", offset)
        self.s.barrier(forbid=("run-pipeline", "played"))
    def disconnect(self):
        self.address = self.s.sock.getpeername()
        self.s.sock.shutdown(socket.SHUT_RDWR)
        self.s.stream.close(); self.s.sock.close()
        self.s.stream = self.s.sock = None
        self.s.logged("client disconnected")
def run_trial(strategy, folder):
    playback = Playback(folder)
    try:
        with Session(folder) as session:
            session.wake()
            trial = Trial(session, playback)
            session.send("audio-start", C.FORMAT)
            for _ in range(30):
                session.send("audio-chunk", C.FORMAT, PAYLOAD)
            session.barrier(forbid=("played", "run-pipeline"))
            with playback.condition:
                if not playback.condition.wait_for(lambda: playback.start is not None, session.remaining()):
                    raise TimeoutError("playback did not start")
            session.context = "pre-wake"
            trial.pump(playback.start + 0.5)
            trial.wake_speaking()
            assert trial.chunk, "pre-wake: expected microphone witness"
            trial.phase()
            session.context = "pre-cut"
            trial.pump(playback.start + 1)
            assert trial.chunk, "pre-cut: expected microphone witness"
            cut = time.monotonic()
            if strategy == "close-stream":
                trial.disconnect()
            elif strategy != "none":
                session.send(strategy)
            # The second wake precedes resume, reconnect and cleanup.
            session.proc.send_signal(signal.SIGUSR1)
            if strategy in ("audio-stop", "none"):
                session.logged("left to the client (-D)", len(session.log))
            else:
                session.capture_barrier()
            if strategy == "pause-satellite":
                # Drain pre-pause microphone bytes without opening a new sample phase.
                # Pong acknowledges pause on the same ordered protocol connection.
                session.acknowledge_pause()
            session.context = "post-cut"
            trial.phase()
            if strategy == "close-stream":
                while time.monotonic() < cut + 4:
                    session.logs(max(0, min(0.05, cut + 4 - time.monotonic())))
            else:
                if strategy == "none":
                    trial.pump(playback.start + 3)
                    session.send("audio-stop")  # Normal completion, not cancellation.
                trial.pump(cut + 4)
            result = {"strategy": strategy, "cut_ms": elapsed(playback.start, cut),
                      "end_ms": None if playback.end is None else elapsed(cut, playback.end),
                      "bytes": len(playback.data), "played": trial.played,
                      "pipelines": trial.pipelines}
            assert_result(strategy, playback, trial, result)
            session.context = "cleanup"
            # Recover only after the measured window. Prove a fresh run separately.
            if strategy == "close-stream":
                session.sock = socket.create_connection(trial.address, timeout=session.remaining())
                session.stream = session.sock.makefile("rb", buffering=0)
                session.send("describe", keep=False)
                session.until("info", keep=False)
            if strategy in ("pause-satellite", "close-stream"):
                session.send("audio-stop")
                session.until("played", forbid=("run-pipeline",))
            session.wake()
            return session.rows, result
    finally:
        playback.close()
def assert_result(strategy, playback, trial, result):
    assert trial.pipelines == 0, f"{strategy}: expected pipelines=0, actual={result}"
    assert bytes(playback.data) == (PAYLOAD * 30)[:len(playback.data)], f"{strategy}: expected reply PCM, actual={result}"
    drained = strategy in ("audio-stop", "none")
    want = (144000, 1, True) if drained else (143360, 0, False)
    actual = (result["bytes"], trial.played, playback.end is not None)
    assert actual == want, f"{strategy}: expected bytes/played/EOF={want}, actual={actual}"
def assert_same(raw, want, message):
    """Fail with the differing wire lines, so a CI-only mismatch names its cause."""
    if raw == want:
        return
    diff = difflib.unified_diff(want.decode(errors="replace").splitlines(), raw.decode(errors="replace").splitlines(),
                                "expected", "observed", lineterm="", n=1)
    shown = [line[:400] for line in list(diff)[:60]]
    raise AssertionError(message + "\n" + "\n".join(shown))
def rows_bytes(rows):
    return b"".join(C.encoded(row) + b"\n" for row in rows)
def publish(output, producer):
    """Stage complete results, preserving originals and rolling back replacement."""
    with C.publication(output):
        with tempfile.TemporaryDirectory(dir=output.parent, prefix=".barge-in-") as temp:
            stage = Path(temp) / "stage"
            if output.exists():
                shutil.copytree(output, stage)
            else:
                stage.mkdir()
            producer(stage)
            backup = Path(temp) / "previous"
            masker = getattr(signal, "pthread_sigmask", lambda *_: set())
            previous = masker(signal.SIG_BLOCK if hasattr(signal, "SIG_BLOCK") else 0, {signal.SIGINT, signal.SIGTERM})
            try:
                if output.exists():
                    output.rename(backup)
                try:
                    stage.rename(output)
                except BaseException:
                    if backup.exists():
                        backup.rename(output)
                    raise
            finally:
                masker(signal.SIG_SETMASK if hasattr(signal, "SIG_SETMASK") else 0, previous)
def measure(stage=None, repetitions=10):
    for strategy in STRATEGIES:
        samples = []
        expected = None
        for index in range(repetitions):
            with tempfile.TemporaryDirectory() as folder:
                rows, result = run_trial(strategy, folder)
            raw = rows_bytes(rows)
            if expected is None:
                expected = raw
            assert_same(raw, expected, f"{strategy}: normalized windows changed between trials")
            if stage is None:
                assert_same(raw, (OUTPUT / f"barge-in-{strategy}.jsonl").read_bytes(), f"{strategy}: fixture mismatch")
            print(C.encoded({"repetition": index + 1, **result}).decode(), flush=True)
            samples.append(result["end_ms"])
            if stage is not None and index == 0:
                (stage / f"barge-in-{strategy}.jsonl").write_bytes(rows_bytes(rows))
        if all(sample is not None for sample in samples):
            median, maximum = summary(samples)
            print(f"{strategy}: repetitions={repetitions} mode=drain bytes=144000 played=1 median_ms={median:.3f} max_ms={maximum:.3f}", flush=True)
        else:
            print(f"{strategy}: repetitions={repetitions} playback sink still open at cut+4000ms; censored, no end latency", flush=True)
class LogicTests(unittest.TestCase):
    def test_semantic_pause_boundary_and_all_receive_paths(self):
        mic = {"type": "audio-chunk", "data": {"rate": 16000, "width": 2, "channels": 1, "timestamp": 42}}
        pong = ({"type": "pong", "data": {}}, b"")
        windows = []
        for queued in (False, True):
            session = Session("unused")
            session.sock, session.proc = mock.Mock(), mock.Mock()
            session.remaining = lambda: 1
            session.mic, session.context = "active", "pause-drain"
            trial = Trial(session, None)
            # Existing active witnesses remain; only queued duplicates are omitted.
            for _ in range(2):
                trial.phase()
                ticks = [0, 0, 2] if queued else [0, 0, 0, 0, 2]
                with mock.patch.object(C, "read_event", return_value=(mic, bytes(1600))), mock.patch.object(time, "monotonic", side_effect=ticks), mock.patch.object(select, "select", return_value=([session.sock], [], [])):
                    trial.pump(1)
            session.send("pause-satellite")
            events = ([(mic, bytes(1600))] if queued else []) + [pong]
            with mock.patch.object(C, "read_event", side_effect=events):
                session.acknowledge_pause()
            session.send("audio-stop")
            with mock.patch.object(C, "read_event", return_value=({"type": "played", "data": {}}, b"")):
                session.until("played")
            windows.append(rows_bytes(session.rows))
        self.assertEqual(*windows)
        fixture = [C.json.loads(line) for line in (OUTPUT / "barge-in-pause-satellite.jsonl").read_bytes().splitlines()]
        cut = next(i for i, row in enumerate(fixture) if row["header"]["type"] == "pause-satellite")
        assert_same(windows[0], rows_bytes(fixture[cut - 2:cut + 3]), "controlled pause window")
        print("pause boundary: queued=0/1 canonical=" + windows[0].decode().strip())
        mutations = [(mic, bytes(1601)), (mic, bytes(100) + b"x" + bytes(1499))]
        for key, value in (("rate", 8000), ("width", 1), ("channels", 2), ("timestamp", "42"), ("extra", 0)):
            mutations.append(({**mic, "data": {**mic["data"], key: value}}, bytes(1600)))
        for context in ("active", "barrier", "pause-drain", "cleanup", "recovery"):
            for event in mutations:
                session.mic, session.context = "active", context
                with mock.patch.object(C, "read_event", return_value=event), self.assertRaisesRegex(AssertionError, context + ": audio-chunk: expected") as error:
                    session.until("pong", keep=False)
        for kind in ("audio-chunk", "audio-start", "run-pipeline", "played", "unknown"):
            session.mic, session.context = "paused", "post-cut"
            event = (mic, bytes(1600)) if kind == "audio-chunk" else ({"type": kind, "data": {}}, b"")
            with mock.patch.object(C, "read_event", return_value=event), self.assertRaisesRegex(AssertionError, "post-cut: " + kind + ": expected") as error:
                session.receive()
        session.stop_pending = True
        with mock.patch.object(C, "read_event", return_value=({"type": "played", "data": {}}, b"")):
            session.until("played")
            with self.assertRaisesRegex(AssertionError, "audio-stop before played"):
                session.until("played")
    def test_lead_in_is_exact_and_precedes_regular_pcm(self):
        session = Session("unused")
        session.sock, session.remaining = mock.Mock(), lambda: 1
        session.mic, session.lead_remaining = "lead", 25600
        event = {"type": "audio-chunk", "data": {"rate": 16000, "width": 2, "channels": 1, "timestamp": 0}}
        for index, size in enumerate([2048] * 12 + [1024, 1600]):
            if index == 1:
                session.send("run-satellite", keep=False)
            with mock.patch.object(C, "read_event", return_value=(event, bytes(size))):
                session.receive("audio-chunk")
        self.assertEqual((session.mic, session.lead_remaining), ("active", 0))
        for data, payload in (({"extra": 1}, b""), ({}, b"x")):
            with mock.patch.object(C, "read_event", return_value=({"type": "pong", "data": data}, payload)), self.assertRaisesRegex(AssertionError, "expected data=.*payload=0"):
                session.until("pong")
        with mock.patch.object(C, "read_event", return_value=(event, bytes(2048))), self.assertRaisesRegex(AssertionError, "expected bytes=1600"):
            session.receive()
    def test_exact_playback_oracle(self):
        for strategy in STRATEGIES:
            drained = strategy in ("audio-stop", "none")
            count, played = (144000, 1) if drained else (143360, 0)
            playback = mock.Mock(data=(PAYLOAD * 30)[:count], end=3 if drained else None)
            trial = mock.Mock(played=played, pipelines=0)
            result = {"bytes": count}
            assert_result(strategy, playback, trial, result)
            for field, value, diagnostic in (("played", 2, "bytes/played/EOF"), ("pipelines", 1, "pipelines=0")):
                with mock.patch.object(trial, field, value), self.assertRaisesRegex(AssertionError, diagnostic):
                    assert_result(strategy, playback, trial, result)
            for field, value, diagnostic in (("end", None if drained else 3, "bytes/played/EOF"), ("data", b"x" * count, "reply PCM")):
                with mock.patch.object(playback, field, value), self.assertRaisesRegex(AssertionError, diagnostic):
                    assert_result(strategy, playback, trial, result)
            with self.assertRaisesRegex(AssertionError, "bytes/played/EOF"):
                assert_result(strategy, playback, trial, {"bytes": count - 1})
    def test_mismatch_names_the_differing_lines(self):
        assert_same(b"a\nb\n", b"a\nb\n", "same")
        with self.assertRaises(AssertionError) as caught:
            assert_same(b"a\nX\n", b"a\nb\n", "pause-satellite: fixture mismatch")
        text = str(caught.exception)
        self.assertTrue(text.startswith("pause-satellite: fixture mismatch\n"), text)
        self.assertIn("-b", text.splitlines())
        self.assertIn("+X", text.splitlines())
    def test_measurement_arithmetic(self):
        self.assertEqual(elapsed(2, 5), 3000)
        self.assertEqual(elapsed(2, None, lambda: 3.5), 1500)
        self.assertEqual(summary([4, 1, 2, 3]), (2.5, 4))
    def test_disconnect_closes_both_owners(self):
        session = mock.Mock(); trial = Trial(session, None)
        sock, stream = session.sock, session.stream
        trial.disconnect()
        sock.shutdown.assert_called_once_with(socket.SHUT_RDWR)
        sock.close.assert_called_once_with(); stream.close.assert_called_once_with()
        self.assertIsNone(trial.s.sock)
        session.logged.assert_called_once_with("client disconnected")
    def test_envelope_matches_real_digest_and_normalization(self):
        row = C.record("bridge->echo", {"type": "audio-chunk", "data": C.FORMAT}, PAYLOAD)
        self.assertEqual(row["header"]["payload_length"], 4800)
        self.assertEqual(row["payload_sha256"], C.hashlib.sha256(PAYLOAD).hexdigest())
        self.assertEqual(C.normalize({"timestamp": 94, "version": "x"}), {"timestamp": 0, "version": "<version>"})
    def test_failure_and_competition_preserve_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"; output.mkdir()
            (output / "original").write_bytes(b"unchanged")
            for error in (TimeoutError, EOFError, InterruptedError, KeyboardInterrupt):
                def fail(stage, failure=error):
                    (stage / "partial").write_bytes(b"partial")
                    raise failure("controlled failure")
                with self.assertRaises(error):
                    publish(output, fail)
                self.assertEqual({p.name: p.read_bytes() for p in output.iterdir()}, {"original": b"unchanged"})
                self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ["fixtures"])
            with C.publication(output):
                with self.assertRaises(FileExistsError):
                    publish(output, lambda _: None)
            publish(output, lambda stage: (stage / "new").write_bytes(b"complete"))
            self.assertEqual((output / "original").read_bytes(), b"unchanged")
            self.assertEqual((output / "new").read_bytes(), b"complete")
            rename = Path.rename
            def failed_rename(source, target):
                if source.name == "stage":
                    raise OSError("controlled rename failure")
                return rename(source, target)
            with mock.patch.object(Path, "rename", failed_rename), self.assertRaises(OSError):
                publish(output, lambda stage: (stage / "partial").write_bytes(b"partial"))
            self.assertEqual(sorted(p.name for p in output.iterdir()), ["new", "original"])
            self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ["fixtures"])
def failure_cli():
    """T3 faults through CLI dispatch, with an actual owned host to reap."""
    for fault in ("timeout", "host-exit", "cancel", "pcm", "format", "control"):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"; output.mkdir()
            (output / "prior").write_bytes(b"published")
            program = """
import importlib.util, os, signal, sys, time
spec = importlib.util.spec_from_file_location('measurement', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
fault = sys.argv[2]
def fail(trial, end):
    print('owned_pid=' + str(trial.s.proc.pid), flush=True)
    if fault in ('pcm', 'format', 'control'):
        from unittest import mock
        trial.s.mic = 'active'
        event = {'type': 'audio-chunk', 'data': {'rate': 16000, 'width': 2, 'channels': 1, 'timestamp': 0}}
        payload = bytes(1600)
        if fault == 'pcm': payload = bytes(100) + b'x' + bytes(1499)
        if fault == 'format': event['data']['rate'] = 8000
        if fault == 'control': event = {'type': 'played', 'data': {}}
        with mock.patch.object(m.C, 'read_event', return_value=(event, payload)):
            trial.s.receive()
    if fault == 'cancel':
        os.kill(os.getpid(), signal.SIGTERM)
    elif fault == 'host-exit':
        trial.s.proc.kill()
        trial.s.proc.wait()
    else:
        trial.s.deadline = time.monotonic() - 1
    trial.s.remaining()
m.Trial.pump = fail
sys.argv = [sys.argv[1], '--output-dir', sys.argv[3]]
sys.exit(m.entry())
"""
            result = subprocess.run([sys.executable, "-c", program, str(Path(__file__).resolve()), fault, str(output)],
                                    capture_output=True, text=True, timeout=25)
            assert result.returncode == 1, result
            diagnostic = {"timeout": "deadline exceeded", "host-exit": "exited", "cancel": "cancelled by signal", "pcm": "expected bytes=1600", "format": "expected format=", "control": "expected audio-chunk, actual=played"}[fault]
            assert diagnostic in result.stderr, result
            pid = int(result.stdout.split("owned_pid=")[1].splitlines()[0])
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise AssertionError(f"owned host {pid} survived {fault}")
            assert {p.name: p.read_bytes() for p in output.iterdir()} == {"prior": b"published"}
            assert sorted(p.name for p in Path(temp).iterdir()) == ["fixtures"]
            print(f"fault={fault}: exit=1 diagnostic={diagnostic}; owned host reaped; output preserved")
def originals():
    before = {p.name: p.read_bytes() for p in OUTPUT.iterdir()}
    with tempfile.TemporaryDirectory() as temp:
        destination = Path(temp) / "fixtures"
        subprocess.run([sys.executable, str(C.ROOT / "scripts/capture-wyoming.py"),
                        "--output-dir", str(destination)], check=True, timeout=90)
        for name in C.SCENARIOS:
            assert (destination / (name + ".jsonl")).read_bytes() == before[name + ".jsonl"]
    assert {p.name: p.read_bytes() for p in OUTPUT.iterdir()} == before
    print("five original fixtures match; default unchanged")
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--unit-test", action="store_true")
    modes.add_argument("--self-test", action="store_true")
    parser.add_argument("--output-dir", type=Path, default=OUTPUT)
    args = parser.parse_args()
    if args.unit_test:
        return 0 if unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(LogicTests)).wasSuccessful() else 1
    if args.self_test:
        originals()
        failure_cli()
        measure(repetitions=1)
    else:
        publish(args.output_dir, measure)
    return 0
def entry():
    def cancelled(signum, _frame):
        raise InterruptedError(f"measurement cancelled by signal {signum}")
    signal.signal(signal.SIGTERM, cancelled)
    try:
        return main()
    except (OSError, ValueError, RuntimeError, AssertionError, EOFError, KeyboardInterrupt) as error:
        print(f"measure-barge-in: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
if __name__ == "__main__":
    raise SystemExit(entry())
