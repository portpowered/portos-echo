"""Private pinned-host owner. JSON stdin, bounded fixed replies, EOF teardown.

Native Linux process owner. The upstream audio-file backend has a real
clock; FIFO consumption and log markers provide readiness without sleep oracles.
"""
import contextlib, ctypes, hashlib, json, os, queue, runpy, select, shutil
from pathlib import Path
import signal, socket, struct, subprocess
import sys
import tempfile
import threading
import time

_m3 = runpy.run_path(str(Path(__file__).with_name("m3") / "host_input.py"))
finite_input, playback_observation = _m3["finite_input"], _m3["playback_observation"]
playback_counts = runpy.run_path(str(Path(__file__).with_name("m2") / "host_playback.py"))["playback_counts"]

ROOT = Path(__file__).resolve().parents[5]
LIMIT = 65536

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

def emit(value):
    print(json.dumps(value), flush=True)

class HostCancelled(RuntimeError):
    """Caller EOF cancelled a pending host command."""

def launcher_input(events):
    try:
        pending = b""
        while True:
            data = os.read(sys.stdin.fileno(), 4096)
            if not data:
                if pending:
                    raise ValueError("invalid command")
                events.put(("eof", None))
                return True
            pending += data
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                if len(line) >= LIMIT:
                    raise ValueError("invalid command")
                events.put(("input", line + b"\n"))
            if len(pending) >= LIMIT:
                raise ValueError("invalid command")
    except (OSError, ValueError):
        events.put(("error", None))
        return False


def launcher_emit(value):
    sys.stdout.buffer.write((json.dumps(value) + "\n").encode("utf-8"))
    sys.stdout.buffer.flush()
class Host:
    def __init__(self, folder, finite_mode=False):
        self.folder = Path(folder)
        self.finite_mode = finite_mode
        self.proc = None
        self.staged = None
        self.stop = threading.Event()
        self.cancelled = threading.Event()
        self.condition = threading.Condition()
        self.log = bytearray()
        self.fixture = bytes(640)
        self.pending = b""
        self.finite = False
        self.fed = 0
        self.fd = None
        self.keys = None
        self.keys_ready = False
        self.muted = False
        self.threads = []
        self.error = False
        self.closed = False
        self.port = None
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
    def logs(self):
        while not self.stop.is_set():
            if not select.select([self.proc.stderr], [], [], 0.1)[0]:
                continue
            data = os.read(self.proc.stderr.fileno(), 4096)
            with self.condition:
                if not data or len(self.log) + len(data) > LIMIT:
                    self.error = True
                    self.condition.notify_all()
                    return
                self.log.extend(data)
                self.keys_ready = self.keys_ready or b"buttons: EVIOCGRAB" in self.log
                self.condition.notify_all()
    def logged(self, marker):
        with self.condition:
            ready = self.condition.wait_for(
                lambda: marker in self.log or self.error or self.cancelled.is_set(), 20)
            if not ready or self.error:
                raise RuntimeError("host readiness failed")
            if self.cancelled.is_set():
                raise HostCancelled("host command cancelled")
            self.log = self.log[self.log.index(marker) + len(marker):]
    def feed(self):
        while not self.stop.is_set():
            with self.condition:
                if self.finite_mode and not self.pending:
                    self.condition.wait_for(lambda: self.pending or self.stop.is_set(), 0.1)
                    continue
            if not select.select([], [self.fd], [], 0.1)[1]:
                continue
            with self.condition:
                data = self.pending or self.fixture
                try:
                    count = os.write(self.fd, data)
                except BlockingIOError:
                    continue
                if self.pending:
                    self.pending = self.pending[count:]
                self.fed += count
                self.condition.notify_all()
    def start(self):
        (self.folder / "state").mkdir(exist_ok=True)
        config = "even_volume=off\n"
        if self.finite_mode:
            config += "wake_sound=off\nwake_sound_removal=off\n"
        (self.folder / "state/config").write_text(config)
        (self.folder / "addrs").write_text("127.0.0.1\n")
        (self.folder / "capture").unlink(missing_ok=True)
        os.mkfifo(self.folder / "capture")
        self.fd = os.open(self.folder / "capture", os.O_RDWR | os.O_NONBLOCK)
        (self.folder / "keys").unlink(missing_ok=True)
        os.mkfifo(self.folder / "keys")
        self.keys = os.open(self.folder / "keys", os.O_RDWR | os.O_NONBLOCK)
        import fcntl  # One-page pipe: a say waits 0.4 s, not 2.3 s, behind queued audio.
        fcntl.fcntl(self.fd, fcntl.F_SETPIPE_SZ, 4096)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = self.port or reservation.getsockname()[1]
        self.port = port
        env = {k: v for k, v in os.environ.items() if not k.startswith("HASSMIC_")}
        env.update(HASSMIC_STATE=str(self.folder / "state"), HASSMIC_CAP=str(self.folder / "capture"),
                   HASSMIC_PLAY=str(self.folder / "play.raw"), HASSMIC_FAKE_LED=str(self.folder / "led"),
                   HASSMIC_ARB_ADDR="127.255.255.255", HASSMIC_ARB_RIVALS="0",
                   HASSMIC_FAKE_ADDRS=str(self.folder / "addrs"), HASSMIC_MDNS_FILE=str(self.folder / "no-mdns"))
        binary = self.stage()
        self.proc = subprocess.Popen(
            [str(binary), "-P", "wyoming", "-D", "-p", str(port),
             "-b", str(self.folder / "keys"), "-n", "Lab Sim " + self.folder.name], cwd=binary.parent, env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True, preexec_fn=parent_death_guard())
        for target in [self.logs, self.feed]:
            thread = threading.Thread(target=target, daemon=True)
            thread.start()
            self.threads.append(thread)
        self.logged(b"wyoming on ")
        with self.condition:
            ready = self.condition.wait_for(
                lambda: self.keys_ready or self.error or self.cancelled.is_set(), 20)
            if not ready or self.error:
                raise RuntimeError("host keypad readiness failed")
            if self.cancelled.is_set():
                raise HostCancelled("host command cancelled")
        emit({"event": "host.ready", "address": "127.0.0.1:" + str(port)})
    def mute(self):
        # Linux input_event uses the guest's native timeval, not the launcher's
        # Windows ABI. KEY_MUTE toggles the host board's software fallback on up.
        events = b"".join(struct.pack("@llHHi", 0, 0, 1, 113, value) for value in [1, 0])
        if self.keys is None or os.write(self.keys, events) != len(events):
            raise RuntimeError("host keypad write failed")
        muted = not self.muted
        self.logged(f"mic mute button: {int(muted)}\n".encode())
        self.muted = muted
    def command(self, value):
        if self.cancelled.is_set():
            if self.error:
                raise RuntimeError("host input failed")
            raise HostCancelled("host command cancelled")
        action = value.get("action")
        if action == "restart":
            previous = self.proc.pid
            self.close()
            self.stop = threading.Event()
            self.threads, self.log = [], bytearray()
            self.closed = self.error = self.keys_ready = self.muted = False
            self.start()
            emit({"event": "host.ack", "previous_pid": previous, "pid": self.proc.pid})
            return
        if action == "identity":
            emit({"event": "host.ack", "pid": self.proc.pid})
            return
        if action == "registered":
            self.logged(b"satellite: running")
        elif action == "mute":
            self.mute()
        elif action == "playback" and self.finite_mode:
            emit(playback_observation(self, LIMIT, playback_counts,
                                      wait=value.get("wait_flush", False),
                                      earcon=value.get("wake_earcon", False)))
            return
        elif action in ("finite", "inject", "muted"):
            finite_input(self, value, signal.SIGUSR1)
        else:
            if action == "say":
                data = bytes(value["pcm"])
                if not data or len(data) > 4096 or len(data) % 2:
                    raise ValueError("invalid fixture")
                import fcntl
                with self.condition:
                    self.fixture = data
                    target = self.fed + fcntl.fcntl(self.fd, fcntl.F_GETPIPE_SZ) + 8192
                    ready = self.condition.wait_for(
                        lambda: self.fed >= target or self.error or self.cancelled.is_set(), 20)
                    if not ready or self.error:
                        raise RuntimeError("fixture consumption failed")
                    if self.cancelled.is_set():
                        raise HostCancelled("host command cancelled")
                action = "wake"
            signals = {"wake": signal.SIGUSR1, "action": signal.SIGUSR2,
                       "pair": signal.SIGWINCH, "stop": signal.SIGHUP}
            self.proc.send_signal(signals[action])
        emit({"event": "host.ack"})
    def close(self):
        if self.closed:
            return
        self.stop.set()
        interrupted = isinstance(sys.exc_info()[1], SystemExit)
        deadline = time.monotonic() + (1.5 if interrupted else 4)
        try:
            if self.proc and self.proc.poll() is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.proc.pid, signal.SIGKILL if interrupted else signal.SIGTERM)
                try:
                    self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(self.proc.pid, signal.SIGKILL)
                    self.proc.wait()
        finally:
            failed = False
            for thread in self.threads:
                thread.join(timeout=max(0, deadline - time.monotonic()))
                failed = failed or thread.is_alive()
            if self.proc:
                self.proc.stderr.close()
            if self.fd is not None:
                os.close(self.fd)
                self.fd = None
            if self.keys is not None:
                os.close(self.keys)
                self.keys = None
            if self.staged is not None:
                self.staged.cleanup()
                self.staged = None
            if failed:
                raise RuntimeError("host thread cleanup failed")
        self.closed = True
def host_input(host, events):
    # Read EOF independently of log/FIFO waits. Only the main owner closes the
    # host: interrupt its wait, then let its finally stop and collect the group
    # before the caller's retained four-second deadline expires.
    clean = False
    try:
        clean = launcher_input(events)
    finally:
        with host.condition:
            host.error = host.error or not clean
            host.cancelled.set()
            host.condition.notify_all()
@controller_signals()
def main():
    failed = False
    with tempfile.TemporaryDirectory(prefix="echo-lab-sim-") as folder:
        host = Host(folder, "--finite" in sys.argv[1:])
        events = queue.Queue()
        try:
            threading.Thread(target=host_input, args=(host, events), daemon=True).start()
            host.start()
            while True:
                action, line = events.get()
                if action == "eof":
                    break
                if action != "input":
                    raise ValueError("invalid command")
                host.command(json.loads(line))
        except HostCancelled:
            pass
        except (OSError, ValueError, KeyError, RuntimeError):
            failed = True
            emit({"event": "host.failed"})
        finally:
            host.close()
    emit({"event": "host.closed"})
    return int(failed)
if __name__ == "__main__":
    sys.exit(main())
