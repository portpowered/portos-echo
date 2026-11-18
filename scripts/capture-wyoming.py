#!/usr/bin/env python3
"""Capture selected real Wyoming event windows, never whole audio payloads.
Each phase retains its first microphone chunk; intervening identical streaming
chunks are consumed and checked, not stored. Timestamps become zero and versions
become <version>; data_length describes the normalized compact JSON data.
"""
import ctypes, runpy
import argparse, base64, contextlib, hashlib, io, json, os, select, shutil, signal
import socket, subprocess, sys, tempfile, threading, time, unittest
from pathlib import Path
from unittest import mock
ROOT = Path(__file__).resolve().parent.parent
SCENARIOS = ("describe", "wake-word", "duplex-reply", "pause", "wake-while-speaking")
FORMAT = {"rate": 24000, "width": 2, "channels": 1}
TIMEOUT = 15
def parent_death_guard():
    """Prepare libc before fork; the child only arms and checks its parent."""
    expected = os.getpid()
    prctl = ctypes.CDLL(None, use_errno=True).prctl
    prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong,
                      ctypes.c_ulong, ctypes.c_ulong]
    prctl.restype = ctypes.c_int
    def arm():
        if prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "parent-death guard failed")
        if os.getppid() != expected:
            os.kill(os.getpid(), signal.SIGKILL)
    return arm


@contextlib.contextmanager
def controller_signals(message="controller cancelled by signal"):
    signals = (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)
    def cancelled(signum, _frame):
        # A second handled signal must not interrupt collection/unstaging.
        for sig in signals:
            signal.signal(sig, signal.SIG_IGN)
        raise SystemExit(f"{message} {signum}")
    previous = {sig: signal.getsignal(sig) for sig in signals}
    try:
        for sig in signals:
            signal.signal(sig, cancelled)
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


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
        self.staged = None
        self.proc = self.sock = self.stream = None
        self.fed = 0
        self.pipe_capacity = 65536
        self.feed_condition = threading.Condition()
        self.stop_feed = threading.Event()
        self.feeder = None
    def stage(self):
        pinned = Path(subprocess.check_output(
            ["sh", str(ROOT / "scripts/hassmic-host.sh"), "path"], text=True,
            timeout=20).strip())
        self.staged = tempfile.TemporaryDirectory(prefix="hassmic-exec-", dir="/tmp")
        folder = Path(self.staged.name).resolve()
        if folder == Path("/mnt") or Path("/mnt") in folder.parents:
            raise RuntimeError("stage requires Linux storage")
        binary = folder / "hassmic-host"
        shutil.copy2(pinned, binary)
        if hashlib.sha256(binary.read_bytes()).digest() != hashlib.sha256(pinned.read_bytes()).digest():
            raise RuntimeError("stage hash mismatch")
        return binary

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
            binary = self.stage()
            self.proc = subprocess.Popen(
                [str(binary), "-P", "wyoming",
                 "-D", "-p", str(port), "-n", "Capture Echo"],
                cwd=binary.parent, env=env, stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE, start_new_session=True, preexec_fn=parent_death_guard())
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
        try:
            if self.proc:
                if self.proc.poll() is None:
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(self.proc.pid, signal.SIGKILL if isinstance(sys.exc_info()[1], SystemExit)
                                  else signal.SIGTERM)
                try:
                    self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(self.proc.pid, signal.SIGKILL)
                    self.proc.wait(timeout=3)
        finally:
            if self.proc:
                self.proc.stderr.close()
            if self.feeder:
                self.feeder.join(timeout=1)
            if hasattr(self, "feed_fd"):
                os.close(self.feed_fd)
            if self.staged is not None:
                self.staged.cleanup()
                self.staged = None
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
            if output.exists():
                shutil.copytree(output, stage)
            else:
                stage.mkdir()
            for kind in SCENARIOS:
                print(f"capture-wyoming: capturing {kind}", flush=True)
                with tempfile.TemporaryDirectory() as session_dir:
                    rows = producer(kind, session_dir)
                validate(rows, kind)
                (stage / (kind + ".jsonl")).write_bytes(b"".join(encoded(r) + b"\n" for r in rows))
            backup = Path(temp) / "previous"
            # Block handled cancellation during the two-rename transaction.
            previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM, signal.SIGHUP})
            try:
                try:
                    if output.exists():
                        output.rename(backup)
                    stage.rename(output)
                finally:
                    # Pending handled signals may raise here: retain rollback until return.
                    signal.pthread_sigmask(signal.SIG_SETMASK, previous)
            except BaseException:
                if backup.exists():
                    if output.exists():
                        shutil.rmtree(output)
                    backup.rename(output)
                elif not stage.exists() and output.exists():
                    shutil.rmtree(output)
                raise
    print("capture-wyoming: five real-wire fixtures published")
def fixture_bytes(output=ROOT / "tests/fixtures/wyoming"):
    return {str(p.relative_to(output)): p.read_bytes() for p in output.rglob("*") if p.is_file()}
def verify_output_cli():
    """T3: real CLI success, contention, transport failure and handled cancellation."""
    before = fixture_bytes()
    script = str(Path(__file__).resolve())
    # Inject only at the socket/host boundary; execute the delivered entry point.
    probe = """import os, runpy, signal, socket, subprocess, sys
from unittest import mock
mode, sys.argv = sys.argv[1], sys.argv[2:]
children = []
original = subprocess.Popen
def start(*a, **kw):
    child = original(*a, **kw)
    if a[0][-1] != "path":
        children.append(child)
    return child
def fail(*a, **kw):
    if mode == 'cancel':
        os.kill(os.getpid(), signal.SIGTERM)
    raise TimeoutError('controlled transport timeout')
try:
    with mock.patch('subprocess.Popen', side_effect=start), mock.patch('socket.SocketIO.readinto', side_effect=fail):
        runpy.run_path(sys.argv[0], run_name='__main__')
finally:
    assert len(children) == 1 and all(c.poll() is not None for c in children), 'host leaked'
    print('owned host reaped', flush=True)
"""
    with tempfile.TemporaryDirectory() as temp:
        destination = Path(temp) / "independent captures" / "wyoming"
        destination.mkdir(parents=True)
        for name, raw in before.items():
            (destination / name).write_bytes(b"stale" if name[:-6] in SCENARIOS else raw)
        old = fixture_bytes(destination)
        args = [script, "--output-dir", str(destination.relative_to(temp))]
        def run(extra):
            return subprocess.run([sys.executable, *extra, *args], cwd=temp, text=True,
                                  capture_output=True, timeout=TIMEOUT * len(SCENARIOS) + 5)
        for mode in ("timeout", "cancel", "lock"):
            with publication(destination) if mode == "lock" else contextlib.nullcontext():
                result = run([] if mode == "lock" else ["-c", probe, mode])
                assert result.returncode != 0, result.stdout
                assert ("File exists" if mode == "lock" else "capture cancelled by signal" if mode == "cancel"
                        else "controlled transport timeout") in result.stderr, result.stderr
                if mode != "lock":
                    assert "owned host reaped" in result.stdout, result.stdout
                else:
                    assert destination.with_name("wyoming.lock").is_dir()
                assert fixture_bytes(destination) == old
            assert sorted(p.name for p in destination.parent.iterdir()) == ["wyoming"]
            print(f"capture-wyoming CLI: {mode} nonzero; previous bytes and child cleanup verified")
        result = run([])
        assert result.returncode == 0, result.stderr
        assert fixture_bytes(destination) == before, "isolated CLI capture differs"
    assert fixture_bytes() == before
    print("capture-wyoming CLI: five refreshed; siblings retained; default unchanged")
@controller_signals("capture cancelled by signal")
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "tests/fixtures/wyoming")
    args = parser.parse_args()
    if args.self_test:
        cases = runpy.run_path(str(ROOT / "crates/echo-lab/src/lab_run/sim/host_tests/capture_wire.py"),
                               init_globals=globals())["WireTests"]
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(cases)
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
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, AssertionError) as error:
        raise SystemExit(f"capture-wyoming: {error}") from error
