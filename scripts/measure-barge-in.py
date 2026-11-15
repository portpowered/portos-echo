#!/usr/bin/env python3
"""Measure pinned -D host cancellation on loopback; never contact a device.

Ten trials per strategy; --self-test runs one and compares bounded wire windows.
Playback FIFO EOF is an independent close endpoint. An open sink at the bounded
observation deadline is censored, never a fabricated playback-end timestamp.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
from unittest import mock
import time
import unittest

SPEC = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("capture-wyoming.py"))
C = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(C)
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


class Trial:
    def __init__(self, session, playback):
        self.s = session
        self.p = playback
        self.chunk = False
        self.played = 0
        self.pipelines = 0

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
                event, payload = C.read_event(self.s.stream)
                kind = event["type"]
                assert kind != "audio-start", "unexpected microphone audio-start"
                if kind == "audio-chunk":
                    assert payload == bytes(1600) and event["data"] == {"rate": 16000, "width": 2, "channels": 1, "timestamp": event["data"]["timestamp"]}
                    if self.chunk:
                        continue
                    self.chunk = True
                elif kind == "played":
                    self.played += 1
                elif kind == "run-pipeline":
                    self.pipelines += 1
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
        self.s.stream.close()
        self.s.sock.close()
        self.s.stream = self.s.sock = None
        self.s.logged("client disconnected")


def run_trial(strategy, folder):
    playback = Playback(folder)
    try:
        with C.Session(folder) as session:
            session.wake()
            trial = Trial(session, playback)
            session.send("audio-start", C.FORMAT)
            for _ in range(30):
                session.send("audio-chunk", C.FORMAT, PAYLOAD)
            session.barrier(forbid=("played", "run-pipeline"))
            with playback.condition:
                if not playback.condition.wait_for(lambda: playback.start is not None, session.remaining()):
                    raise TimeoutError("playback did not start")
            trial.pump(playback.start + 0.5)
            trial.wake_speaking()
            trial.phase()
            trial.pump(playback.start + 1)
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
            assert trial.pipelines == 0, result
            assert bytes(playback.data) == (PAYLOAD * 30)[:len(playback.data)], result
            if strategy in ("audio-stop", "none"):
                assert playback.end is not None and result["bytes"] == 144000 and trial.played == 1, result
            else:
                assert playback.end is None and result["bytes"] == 143360 and trial.played == 0, result
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
            assert raw == expected, f"{strategy}: normalized windows changed between trials"
            if stage is None:
                assert raw == (OUTPUT / f"barge-in-{strategy}.jsonl").read_bytes(), f"{strategy}: fixture mismatch"
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
    def test_measurement_arithmetic(self):
        self.assertEqual(elapsed(2, 5), 3000)
        self.assertEqual(elapsed(2, None, lambda: 3.5), 1500)
        self.assertEqual(summary([4, 1, 2, 3]), (2.5, 4))

    def test_disconnect_closes_both_owners(self):
        session = mock.Mock()
        trial = Trial(session, None)
        sock, stream = session.sock, session.stream
        trial.disconnect()
        sock.shutdown.assert_called_once_with(socket.SHUT_RDWR)
        sock.close.assert_called_once_with()
        stream.close.assert_called_once_with()
        self.assertIsNone(trial.s.sock)
        session.logged.assert_called_once_with("client disconnected")

    def test_envelope_matches_real_digest_and_normalization(self):
        row = C.record("bridge->echo", {"type": "audio-chunk", "data": C.FORMAT}, PAYLOAD)
        self.assertEqual(row["header"]["payload_length"], 4800)
        self.assertEqual(row["payload_sha256"], C.hashlib.sha256(PAYLOAD).hexdigest())
        self.assertEqual(C.normalize({"timestamp": 94, "version": "x"}), {"timestamp": 0, "version": "<version>"})

    def test_failure_and_competition_preserve_publication(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"
            output.mkdir()
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
    for fault in ("timeout", "host-exit", "cancel"):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"
            output.mkdir()
            (output / "prior").write_bytes(b"published")
            program = """
import importlib.util, os, signal, sys, time
spec = importlib.util.spec_from_file_location('measurement', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
fault = sys.argv[2]
def fail(trial, end):
    print('owned_pid=' + str(trial.s.proc.pid), flush=True)
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
            diagnostic = {"timeout": "deadline exceeded", "host-exit": "exited", "cancel": "cancelled by signal"}[fault]
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
