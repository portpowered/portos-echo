#!/usr/bin/env python3
"""Measure pinned -D host cancellation on loopback; never contact a device.
Ten trials per strategy; --self-test runs one and compares bounded wire windows.
Playback FIFO EOF is an independent close endpoint. An open sink at the bounded
observation deadline is censored, never a fabricated playback-end timestamp.
"""
import argparse, contextlib, difflib, importlib.util, os, select, shutil, signal, socket
import statistics, subprocess, sys, tempfile, threading, time
from pathlib import Path
SPEC = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("capture-wyoming.py"))
C = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(C)
STRATEGIES = ("audio-stop", "pause-satellite", "close-stream", "none")
OUTPUT = C.ROOT / "tests/fixtures/wyoming"
PAYLOAD = b"\x01\x00" * 2400
SPEAKER = "hassmic-speaker"
def speaker_report(label, data, payload):
    """hassmic reports its speaker on connecting (before info) and on any volume or output-mute change.
    A valid report is device state, not part of a measured window; a malformed one still fails."""
    valid = (set(data) == {"volume", "muted"} and type(data["volume"]) is int and 0 <= data["volume"] <= 100
             and type(data["muted"]) is bool and not payload)
    assert valid, f"{label}: expected volume=0..100 muted=bool payload=0, actual data={data} payload={len(payload)}"
def retain(error, session=None, playback=None, trial=None, **fields):
    """Keep the earliest observed state, before resource cleanup changes it."""
    state = getattr(error, "trial_state", {})
    if session is not None:
        fields.setdefault("phase", session.context)
        fields.update(mic=session.mic, lead_remaining=getattr(session, "lead_remaining", "unknown"))
        try:
            code = session.proc.poll() if session.proc is not None else "unknown"
            fields["host"] = "alive" if code is None else f"exited({code})" if type(code) is int else "unknown"
        except (OSError, ValueError):
            fields["host"] = "unknown"
    if playback is not None:
        fields.update(pcm_bytes=len(playback.data), playback_started=playback.start is not None,
                      playback_eof=playback.end is not None)
    if trial is not None:
        fields.update(played=trial.played, pipelines=trial.pipelines)
    error.trial_state = {**fields, **state}

@contextlib.contextmanager
def boundary(**fields):
    try:
        yield
    except BaseException as error:
        retain(error, **fields)
        raise

def cleanup(action):
    original = sys.exc_info()[1]
    try:
        action()
    except BaseException as error:
        if original is None:
            raise
        original.cleanup_errors = getattr(original, "cleanup_errors", []) + [type(error).__name__ + ": " + (str(error) or "no message")]

def report(error):
    reason = str(error) or ("assertion failed without message" if isinstance(error, AssertionError) else "failure without message")
    print(f"measure-barge-in: {type(error).__name__}: {reason}", file=sys.stderr)
    keys = ("strategy", "repetition", "phase", "pcm_bytes", "played", "pipelines",
            "playback_started", "playback_eof", "host", "mic", "lead_remaining")
    state = getattr(error, "trial_state", {})
    print("trial: " + " ".join(f"{key}={str(state.get(key, 'unknown')).lower()}" for key in keys), file=sys.stderr)
    for secondary in getattr(error, "cleanup_errors", []):
        print("cleanup: " + secondary, file=sys.stderr)

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
    def __exit__(self, *details):
        error = details[1] or sys.exc_info()[1]
        if error is not None:
            retain(error, session=self)
        parent_exit = super().__exit__
        try:
            cleanup(lambda: parent_exit(*details))
        except BaseException as failure:
            retain(failure, session=self, phase="cleanup")
            raise
    def send(self, kind, data=None, payload=b"", keep=True):
        if kind in ("audio-stop", "pause-satellite"):
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
        if kind == SPEAKER:
            # Callers skip it: until() keeps waiting, pump() and wake() read on.
            speaker_report(label, data, payload)
        elif kind == "audio-chunk":
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
                assert self.stop_pending, f"{label}: expected audio-stop, pause-satellite or active hassmic-end before played"
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
        self.until("played", forbid=("run-pipeline",))
        self.barrier(forbid=("played", "run-pipeline"))
        self.mic = "paused"
    def end_run(self, reply_active=False):
        self.context = "cleanup/end"
        self.stop_pending = reply_active
        self.send("hassmic-end")
        if reply_active:
            self.until("played", forbid=("run-pipeline",))
        self.barrier(forbid=("played", "run-pipeline"))
        self.mic = "idle"
    def wake(self):
        self.context = "recovery/wake"
        self.send("run-satellite")
        self.barrier()
        self.proc.send_signal(signal.SIGUSR1)
        self.until("run-pipeline")
        event, payload = self.receive("audio-chunk")
        while event["type"] == SPEAKER:
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
                if kind == SPEAKER:
                    continue
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
    playback = session = trial = None
    try:
        playback = Playback(folder)
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
            cut_bytes = len(playback.data)
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
                # Played closes the aborted reply; pong acknowledges the pause.
                session.acknowledge_pause()
                trial.played += 1
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
                      "cut_bytes": cut_bytes,
                      "pipelines": trial.pipelines}
            assert_result(strategy, playback, trial, result)
            session.context = "cleanup"
            # Recover only after the measured window. Prove a fresh run separately.
            if strategy == "close-stream":
                session.sock = socket.create_connection(trial.address, timeout=session.remaining())
                session.stream = session.sock.makefile("rb", buffering=0)
                session.send("describe", keep=False)
                session.until("info", keep=False)
            session.end_run()
            session.wake()
            return session.rows, result
    except BaseException as error:
        retain(error, session, playback, trial, strategy=strategy)
        raise
    finally:
        if playback is not None:
            try:
                cleanup(playback.close)
            except BaseException as error:
                retain(error, session, playback, trial, strategy=strategy, phase="cleanup")
                raise
def assert_result(strategy, playback, trial, result):
    assert trial.pipelines == 0, f"{strategy}: expected pipelines=0, actual={result}"
    assert bytes(playback.data) == (PAYLOAD * 30)[:len(playback.data)], f"{strategy}: expected reply PCM, actual={result}"
    drained = strategy in ("audio-stop", "none")
    if drained:
        assert result["bytes"] == 144000, f"{strategy}: expected bytes/played/EOF: drained 144000 bytes, actual={result}"
    else:
        # One in-flight 20 ms slot plus the host's 4096-byte stdio buffer can
        # become visible after the cut snapshot. Queued seconds must be dropped.
        assert result["cut_bytes"] <= result["bytes"] <= result["cut_bytes"] + 4096 + 960 < 144000, f"{strategy}: expected bounded abort PCM, actual={result}"
    want = (1 if strategy != "close-stream" else 0, True)
    actual = (trial.played, playback.end is not None)
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
    with boundary(phase="publication"), C.publication(output):
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
            with boundary(strategy=strategy, repetition=index + 1):
                raw = measure_trial(stage, strategy, index, expected, samples)
                if expected is None:
                    expected = raw
        if all(sample is not None for sample in samples):
            median, maximum = summary(samples)
            mode = "drain" if strategy in ("audio-stop", "none") else "abort"
            print(f"{strategy}: repetitions={repetitions} mode={mode} median_ms={median:.3f} max_ms={maximum:.3f}", flush=True)
        else:
            print(f"{strategy}: repetitions={repetitions} playback sink still open at cut+4000ms; censored, no end latency", flush=True)
def measure_trial(stage, strategy, index, expected, samples):
    with boundary(phase="trial-startup"), tempfile.TemporaryDirectory() as folder:
        rows, result = run_trial(strategy, folder)
    with boundary(phase="fixture-comparison", pcm_bytes=result["bytes"], played=result["played"],
                  pipelines=result["pipelines"], playback_started=True, playback_eof=result["end_ms"] is not None):
        raw = rows_bytes(rows)
        assert_same(raw, raw if expected is None else expected, f"{strategy}: normalized windows changed between trials")
        if stage is None:
            assert_same(raw, (OUTPUT / f"barge-in-{strategy}.jsonl").read_bytes(), f"{strategy}: fixture mismatch")
        print(C.encoded({"repetition": index + 1, **result}).decode(), flush=True)
        samples.append(result["end_ms"])
        if stage is not None and index == 0:
            (stage / f"barge-in-{strategy}.jsonl").write_bytes(rows_bytes(rows))
    return raw

def failure_cli():
    """T3 faults through CLI dispatch, with an actual owned host to reap."""
    for fault in ("timeout", "host-exit", "cancel", "pcm", "format", "control", "empty"):
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
    if fault == 'empty':
        trial.s.context = 'post-cut'
        raise AssertionError()
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
            diagnostic = {"timeout": "deadline exceeded", "host-exit": "exited", "cancel": "cancelled by signal", "pcm": "expected bytes=1600", "format": "expected format=", "control": "expected audio-chunk, actual=played", "empty": "assertion failed without message"}[fault]
            assert diagnostic in result.stderr, result
            assert "trial: strategy=audio-stop repetition=1 phase=" in result.stderr, result
            if fault == "empty":
                assert "phase=post-cut" in result.stderr and "host=alive" in result.stderr, result
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
        spec = importlib.util.spec_from_file_location("measurement_tests", Path(__file__).with_name("measure_barge_in_tests.py"))
        tests = importlib.util.module_from_spec(spec); spec.loader.exec_module(tests)
        return tests.run(sys.modules[__name__])
    if args.self_test:
        with boundary(phase="original-fixtures"):
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
    except (Exception, KeyboardInterrupt) as error:
        report(error)
        return 1
if __name__ == "__main__":
    raise SystemExit(entry())
