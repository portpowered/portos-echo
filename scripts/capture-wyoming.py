#!/usr/bin/env python3
"""Capture selected real Wyoming event windows, never whole audio payloads.

Each phase retains its first microphone chunk; intervening identical streaming
chunks are consumed and checked, not stored. Timestamps become zero and versions
become <version>; data_length describes the normalized compact JSON data.
SIGUSR1 is a host test control, not a fabricated Wyoming event.
"""
import argparse
import base64
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCENARIOS = ("describe", "wake-word", "duplex-reply", "pause", "wake-while-speaking")
FORMAT = {"rate": 24000, "width": 2, "channels": 1}
TIMEOUT = 15


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def exact(stream, length):
    parts = bytearray()
    while len(parts) < length:
        part = stream.read(length - len(parts))
        if not part:
            raise EOFError("truncated Wyoming event")
        parts.extend(part)
    return bytes(parts)


def read_event(stream):
    line = stream.readline(65537)
    if not line.endswith(b"\n") or len(line) > 65536:
        raise EOFError("missing or oversized Wyoming header")
    head = json.loads(line)
    if not isinstance(head.get("type"), str):
        raise ValueError("Wyoming type must be a string")
    lengths = [head.get(key, 0) for key in ("data_length", "payload_length")]
    if any(type(n) is not int or not 0 <= n <= 1048576 for n in lengths):
        raise ValueError("invalid Wyoming length")
    data = dict(head.get("data", {}))
    if lengths[0]:
        data.update(json.loads(exact(stream, lengths[0])))
    payload = exact(stream, lengths[1])
    return {"type": head["type"], "data": data}, payload


def normalize(value):
    if isinstance(value, dict):
        return {k: (0 if k == "timestamp" else "<version>" if k == "version"
                    else normalize(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize(v) for v in value]
    return value


def record(direction, event, payload=b""):
    event = normalize(event)
    result = {"dir": direction, "header": {**event,
              "data_length": len(encoded(event["data"])) if event["data"] else 0,
              "payload_length": len(payload)}}
    if payload:
        result.update(payload_sha256=hashlib.sha256(payload).hexdigest(),
                      payload_prefix_b64=base64.b64encode(payload[:32]).decode())
    return result


class Session:
    """One prebuilt host, one loopback connection and an owned process group."""
    def __init__(self, folder):
        self.folder = Path(folder)
        self.rows = []
        self.log = bytearray()
        self.deadline = time.monotonic() + TIMEOUT
        self.proc = self.sock = self.stream = None
        self.fed = 0
        self.pipe_capacity = 65536
        self.feed_condition = threading.Condition()
        self.stop_feed = threading.Event()
        self.feeder = None

    def feed(self):
        import fcntl  # Capture runs on Linux/WSL; offline wire tests are portable.
        try:
            with os.fdopen(os.dup(self.feed_fd), "wb", buffering=0) as pipe:
                self.pipe_capacity = fcntl.fcntl(pipe, fcntl.F_GETPIPE_SZ)
                while not self.stop_feed.is_set():
                    if not select.select([], [pipe], [], 0.1)[1]:
                        continue
                    count = pipe.write(bytes(1600))
                    with self.feed_condition:
                        self.fed += count
                        self.feed_condition.notify_all()
        except BrokenPipeError:
            pass

    def capture_barrier(self):
        # More than pipe capacity + stdio buffering + two capture blocks consumed
        # proves the capture thread has handled its pending SIGUSR1. No sleep oracle.
        with self.feed_condition:
            target = self.fed + self.pipe_capacity + 8192
            if not self.feed_condition.wait_for(lambda: self.fed >= target, self.remaining()):
                raise TimeoutError("capture input consumption stalled")

    def remaining(self):
        if self.proc is not None and self.proc.poll() is not None:
            raise RuntimeError(f"hassmic-host exited {self.proc.returncode}")
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("hassmic-host scenario deadline exceeded")
        return remaining

    def __enter__(self):
        try:
            state = self.folder / "state"
            state.mkdir()
            (state / "config").write_text("even_volume=off\n")
            (self.folder / "addrs").write_text("127.0.0.1\n")
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            env = {k: v for k, v in os.environ.items() if not k.startswith("HASSMIC_")}
            env.update(HASSMIC_STATE=str(state), HASSMIC_PLAY=str(self.folder / "play.raw"),
                       HASSMIC_FAKE_LED=str(self.folder / "led"), HASSMIC_ARB_ADDR="127.255.255.255",
                       HASSMIC_FAKE_ADDRS=str(self.folder / "addrs"),
                       HASSMIC_MDNS_FILE=str(self.folder / "no-mdns"))
            os.mkfifo(self.folder / "capture.fifo")
            # Keep the writer's open cancellable even if child startup fails.
            self.feed_fd = os.open(self.folder / "capture.fifo", os.O_RDWR | os.O_NONBLOCK)
            env["HASSMIC_CAP"] = str(self.folder / "capture.fifo")
            self.feeder = threading.Thread(target=self.feed, daemon=True)
            self.feeder.start()
            self.proc = subprocess.Popen(
                ["sh", str(ROOT / "scripts/hassmic-host.sh"), "run", "-P", "wyoming",
                 "-D", "-p", str(port), "-n", "Capture Echo"],
                cwd=self.folder, env=env, stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE, start_new_session=True)
            while self.sock is None:
                self.remaining()
                try:
                    self.sock = socket.create_connection(("127.0.0.1", port), timeout=1)
                except ConnectionRefusedError:
                    self.logs(0.05)
            self.sock.settimeout(self.remaining())
            self.stream = self.sock.makefile("rb", buffering=0)
            # A response proves this is our freshly named satellite, not another listener.
            self.send("describe", keep=False)
            info = self.until("info", keep=False)
            if info["header"]["data"]["satellite"]["name"] != "Capture Echo":
                raise RuntimeError("unexpected satellite identity")
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *_):
        self.stop_feed.set()
        if self.stream:
            self.stream.close()
        if self.sock:
            self.sock.close()
        if self.proc:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(timeout=3)
            self.proc.stderr.close()
        if self.feeder:
            self.feeder.join(timeout=1)
        if hasattr(self, "feed_fd"):
            os.close(self.feed_fd)

    def logs(self, ceiling=0):
        if select.select([self.proc.stderr], [], [], min(ceiling, self.remaining()))[0]:
            self.log.extend(os.read(self.proc.stderr.fileno(), 65536))

    def logged(self, text, offset=0):
        while text.encode() not in self.log[offset:]:
            self.logs(self.remaining())

    def send(self, kind, data=None, payload=b"", keep=True):
        event = {"type": kind, "data": data or {}}
        raw = encoded(data) if data else b""
        head = {"type": kind, "data_length": len(raw), "payload_length": len(payload)}
        self.sock.sendall(encoded(head) + b"\n" + raw + payload)
        if keep:
            self.rows.append(record("bridge->echo", event, payload))

    def until(self, kind, keep=True, forbid=()):
        while True:
            self.sock.settimeout(self.remaining())
            event, payload = read_event(self.stream)
            if event["type"] == "audio-start":
                raise AssertionError("pinned host must not send microphone audio-start")
            if event["type"] in forbid:
                raise AssertionError(f"unexpected {event['type']} while awaiting {kind}")
            if event["type"] == kind:
                row = record("echo->bridge", event, payload)
                if keep:
                    self.rows.append(row)
                return row

    def barrier(self, forbid=()):
        self.send("ping", keep=False)
        self.until("pong", keep=False, forbid=forbid)

    def wake(self):
        self.send("run-satellite")
        self.barrier()
        self.proc.send_signal(signal.SIGUSR1)
        self.until("run-pipeline")
        # This pin sends audio-chunk directly, without microphone audio-start.
        self.sock.settimeout(self.remaining())
        event, payload = read_event(self.stream)
        if event["type"] != "audio-chunk" or not payload:
            raise AssertionError("run-pipeline must be followed directly by a nonempty audio-chunk")
        self.rows.append(record("echo->bridge", event, payload))

    def reply(self, wake_during=False):
        self.send("audio-start", FORMAT)
        for _ in range(10):
            self.send("audio-chunk", FORMAT, b"\x01\x00" * 2400)
        self.barrier(forbid=("played", "run-pipeline"))
        self.until("audio-chunk", forbid=("played", "run-pipeline"))
        if wake_during:
            # Buffered host playback becomes visible while the one-second reply plays.
            play = self.folder / "play.raw"
            while not play.exists() or play.stat().st_size == 0:
                self.until("audio-chunk", keep=False, forbid=("played", "run-pipeline"))
            before = play.stat().st_size
            self.logs()
            offset = len(self.log)
            self.proc.send_signal(signal.SIGUSR1)
            self.logged("left to the client (-D)", offset)
            self.barrier(forbid=("played", "run-pipeline"))
            self.until("audio-chunk", forbid=("played", "run-pipeline"))
            while play.stat().st_size <= before:
                self.until("audio-chunk", keep=False, forbid=("played", "run-pipeline"))
        self.send("audio-stop")
        self.until("played", forbid=("run-pipeline",))


def capture(kind, folder):
    with Session(folder) as session:
        if kind == "describe":
            session.send("describe")
            session.until("info")
        elif kind == "pause":
            session.send("run-satellite")
            session.barrier()
            session.send("pause-satellite")
            session.barrier()
            session.proc.send_signal(signal.SIGUSR1)
            session.capture_barrier()
            session.send("ping")
            session.until("pong", forbid=("run-pipeline", "audio-chunk"))
            session.wake()
        else:
            session.wake()
            if kind != "wake-word":
                session.reply(kind == "wake-while-speaking")
        return session.rows


def validate(rows, kind):
    for row in rows:
        keys = {"dir", "header"}
        head = row.get("header", {})
        if set(head) != {"type", "data", "data_length", "payload_length"}:
            raise ValueError("invalid fixture header")
        if head["payload_length"]:
            keys |= {"payload_sha256", "payload_prefix_b64"}
        if set(row) != keys:
            raise ValueError("invalid fixture envelope; full payloads are forbidden")
        if row["dir"] not in ("echo->bridge", "bridge->echo"):
            raise ValueError("invalid direction")
        if head["data_length"] != (len(encoded(head["data"])) if head["data"] else 0):
            raise ValueError("invalid normalized data_length")
        if head["payload_length"]:
            prefix = base64.b64decode(row["payload_prefix_b64"], validate=True)
            if len(prefix) != min(32, head["payload_length"]):
                raise ValueError("invalid payload prefix")
            if len(row["payload_sha256"]) != 64:
                raise ValueError("invalid digest")
            bytes.fromhex(row["payload_sha256"])
    incoming = [r["header"]["type"] for r in rows if r["dir"] == "echo->bridge"]
    if kind == "describe":
        assert incoming == ["info"]
        assert rows[-1]["header"]["data"]["satellite"]["name"] == "Capture Echo"
    else:
        assert "audio-start" not in incoming
        assert incoming.count("run-pipeline") == 1
        assert incoming.index("run-pipeline") + 1 == incoming.index("audio-chunk")
        chunks = [r for r in rows if r["header"]["type"] == "audio-chunk" and r["dir"] == "echo->bridge"]
        assert all(r["header"]["data"]["rate"] == 16000 and r["header"]["payload_length"] > 0 for r in chunks)
        if kind in ("duplex-reply", "wake-while-speaking"):
            assert incoming[-1] == "played"
            sent = [r for r in rows if r["dir"] == "bridge->echo" and r["header"]["type"] == "audio-chunk"]
            assert len(sent) == 10 and all(r["header"]["data"] == FORMAT and r["header"]["payload_length"] == 4800 for r in sent)
        if kind == "pause":
            assert incoming[0] == "pong"


@contextlib.contextmanager
def publication(output):
    """Serialize same-output writers; roll back a failed directory replacement."""
    output.parent.mkdir(parents=True, exist_ok=True)
    lock = output.with_name(output.name + ".lock")
    lock.mkdir()  # Existing owner is an explicit error; never steal another capture.
    try:
        yield lock
    finally:
        lock.rmdir()


def publish(output, producer=capture):
    with publication(output):
        with tempfile.TemporaryDirectory(dir=output.parent, prefix=".wyoming-") as temp:
            stage = Path(temp) / "fixtures"
            stage.mkdir()
            for kind in SCENARIOS:
                print(f"capture-wyoming: capturing {kind}", flush=True)
                with tempfile.TemporaryDirectory() as session_dir:
                    rows = producer(kind, session_dir)
                validate(rows, kind)
                (stage / (kind + ".jsonl")).write_bytes(b"".join(encoded(r) + b"\n" for r in rows))
            backup = Path(temp) / "previous"
            # Block handled cancellation during the two-rename transaction.
            previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
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
                signal.pthread_sigmask(signal.SIG_SETMASK, previous)
    print("capture-wyoming: five real-wire fixtures published")


class WireTests(unittest.TestCase):
    def test_fragmented_utf8_and_payload(self):
        class Fragmented(io.BytesIO):
            def read(self, n=-1):
                return super().read(min(n, 1))
        data = {"text": "\u00e9"}
        payload = bytes(range(80))
        wire = encoded({"type": "test", "data_length": len(encoded(data)), "payload_length": 80})
        event, actual = read_event(Fragmented(wire + b"\n" + encoded(data) + payload))
        self.assertEqual(event, {"type": "test", "data": data})
        self.assertEqual(actual, payload)
        row = record("echo->bridge", event, actual)
        self.assertEqual(row["payload_sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(base64.b64decode(row["payload_prefix_b64"]), payload[:32])

    def test_header_only_inline_and_invalid_lengths(self):
        self.assertEqual(read_event(io.BytesIO(b'{"type":"pong"}\n')), ({"type": "pong", "data": {}}, b""))
        self.assertEqual(read_event(io.BytesIO(b'{"type":"x","data":{"a":1}}\n'))[0]["data"], {"a": 1})
        for n in (-1, 1048577, True, 1.5):
            with self.assertRaises(ValueError):
                read_event(io.BytesIO(encoded({"type": "x", "payload_length": n}) + b"\n"))
        for raw in (b"", b'{"type":"x","data_length":2}\n{', b'{"type":"x","payload_length":2}\nx'):
            with self.assertRaises(EOFError):
                read_event(io.BytesIO(raw))

    def test_normalization_and_zero_payload(self):
        value = {"timestamp": 123, "satellite": {"version": "volatile"}}
        self.assertEqual(normalize(value), {"timestamp": 0, "satellite": {"version": "<version>"}})
        self.assertEqual(normalize(normalize(value)), normalize(value))
        self.assertEqual(record("echo->bridge", {"type": "pong", "data": {}}),
                         {"dir": "echo->bridge", "header": {"type": "pong", "data": {}, "data_length": 0, "payload_length": 0}})

    def test_failed_and_competing_capture_preserves_output(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "fixtures"
            output.mkdir()
            (output / "prior").write_bytes(b"complete")
            for error in (EOFError, TimeoutError, KeyboardInterrupt):
                def failed(*_, failure=error):
                    raise failure("capture interrupted")
                with self.assertRaises(error):
                    publish(output, failed)
            with publication(output):
                with self.assertRaises(FileExistsError):
                    publish(output, failed)
            self.assertEqual((output / "prior").read_bytes(), b"complete")
            self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ["fixtures"])

    def test_committed_envelopes(self):
        output = ROOT / "tests/fixtures/wyoming"
        for kind in SCENARIOS:
            rows = [json.loads(line) for line in (output / (kind + ".jsonl")).read_bytes().splitlines()]
            validate(rows, kind)

    def test_flattened_or_extra_header_fields_fail(self):
        row = record("echo->bridge", {"type": "info", "data": {}})
        for changed in ({"dir": row["dir"], **row["header"]},
                        {**row, "type": "info"},
                        {**row, "header": {**row["header"], "payload": "forbidden"}}):
            with self.assertRaises(ValueError):
                validate([changed], "describe")

    def test_microphone_start_and_interrupted_wake_sequence_fail(self):
        output = ROOT / "tests/fixtures/wyoming"
        for kind in SCENARIOS[1:]:
            rows = [json.loads(line) for line in (output / (kind + ".jsonl")).read_bytes().splitlines()]
            pipeline = next(i for i, row in enumerate(rows) if row["header"]["type"] == "run-pipeline")
            for kind_added in ("audio-start", "pong"):
                changed = rows[:pipeline + 1] + [record("echo->bridge", {"type": kind_added, "data": {}})] + rows[pipeline + 1:]
                with self.subTest(scenario=kind, inserted=kind_added), self.assertRaises(AssertionError):
                    validate(changed, kind)


def verify_output_cli():
    """T3: exercise the delivered CLI against the prebuilt host in isolation."""
    default = ROOT / "tests/fixtures/wyoming"
    before = {p.name: p.read_bytes() for p in default.iterdir()}
    with tempfile.TemporaryDirectory() as temp:
        destination = Path(temp) / "independent captures" / "wyoming"
        subprocess.run([sys.executable, str(Path(__file__).resolve()),
                        "--output-dir", str(destination.relative_to(temp))],
                       cwd=temp, check=True, timeout=TIMEOUT * len(SCENARIOS) + 5)
        actual = {p.name: p.read_bytes() for p in destination.iterdir()}
        assert actual == before, "isolated CLI capture differs from committed fixtures"
    assert {p.name: p.read_bytes() for p in default.iterdir()} == before
    print("capture-wyoming CLI: five isolated fixtures match; default unchanged")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "tests/fixtures/wyoming")
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(WireTests)
        return 0 if unittest.TextTestRunner().run(suite).wasSuccessful() else 1
    if args.smoke:
        with tempfile.TemporaryDirectory() as temp:
            with Session(temp):
                print("hassmic-host smoke: info satellite=Capture Echo")
        verify_output_cli()
    else:
        publish(args.output_dir)
    return 0


if __name__ == "__main__":
    def cancelled(signum, _frame):
        raise InterruptedError(f"capture cancelled by signal {signum}")
    signal.signal(signal.SIGTERM, cancelled)
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, AssertionError) as error:
        raise SystemExit(f"capture-wyoming: {error}") from error
