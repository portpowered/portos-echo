"""T3 controller for one prebuilt pinned host, never a hardware device.

stdin commands own the lifecycle; EOF (including a Rust panic) reaps the group.
Only fixed status lines cross stdout. Temporary synthetic audio is deleted.
"""
import contextlib
from concurrent.futures import Future
import ctypes
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import subprocess
from subprocess import Popen as _LockWaiter
import sys
import tempfile
import threading
import time
import weakref

ROOT = Path(__file__).resolve().parents[1]
# A fork must not inherit another start's writable executable descriptor.
# Release before readiness: independent children still start concurrently.
LAUNCH_GUARD = threading.Lock()
TAIL = 8192
# Python fork test helpers must not extend another controller's reservation or
# lock lifetime. Executed children also close these non-inheritable descriptors.
_inherited = weakref.WeakSet()
os.register_at_fork(after_in_child=lambda: [owner.close() for owner in tuple(_inherited)])


@contextlib.contextmanager
def _allocation_guard(deadline):
    # flock belongs to the open file description shared with this short helper.
    # The helper makes acquisition cancellable by the original startup deadline,
    # including when callers run in threads. Never unlink this coordination file.
    path = f'/tmp/wyoming-t3-bind-{os.getuid()}.lock'
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    owner = os.fdopen(fd, 'rb')
    _inherited.add(owner)
    try:
        with LAUNCH_GUARD:
            waiter = _LockWaiter(
                [sys.executable, '-c',
                 'import fcntl,sys; fcntl.flock(int(sys.argv[1]), fcntl.LOCK_EX)', str(fd)],
                pass_fds=(fd,), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            if waiter.wait(timeout=max(0, deadline - time.monotonic())) != 0:
                raise RuntimeError('endpoint coordination failed')
        finally:
            if waiter.poll() is None:
                waiter.kill()
                waiter.wait()
        yield
    finally:
        owner.close()


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


def safe_tail(raw, truncated=False):
    raw = bytes(raw)
    if truncated:
        raw = raw.partition(b"\n")[2]
    records = raw.split(b"\n")
    result = []
    for line in records[:-1]:
        lower = line.lower()
        unsafe = any(word in lower for word in (
            b"bearer", b"token", b"secret", b"private", b"password", b"api_key",
            b"api-key", b"account", b"/", b"\\"))
        unsafe |= any(char < 32 or char > 126 for char in line)
        result.append("<redacted>" if unsafe else line.decode("ascii"))
    if records[-1]:
        result.append("<redacted>")  # Incomplete records have unknown context.
    return "\n".join(result) or "<empty>"


def _exact(stream, length):
    data = stream.read(length) if length else b""
    if len(data) != length:
        raise EOFError("truncated Wyoming event")
    return data


def describe(conn):
    """Send describe on a connected host socket and return info's data.

    The pin reports its speaker on every connection, ahead of info, and again on
    any volume or output-mute change: a well-formed hassmic-speaker is consumed.
    Any other event before info is a failure. Reads unbuffered, so bytes after
    info stay on the socket for the caller.
    """
    conn.sendall(b'{"type":"describe"}\n')
    with conn.makefile("rb", buffering=0) as stream:
        while True:
            line = stream.readline(65537)
            if not line.endswith(b"\n"):
                raise EOFError("missing or oversized Wyoming header")
            head = json.loads(line)
            data = dict(head.get("data") or {})
            if head.get("data_length"):
                data.update(json.loads(_exact(stream, head["data_length"])))
            payload = _exact(stream, head.get("payload_length", 0))
            kind = head.get("type")
            if kind == "info":
                return data
            speaker = (kind == "hassmic-speaker" and set(data) == {"volume", "muted"}
                       and type(data["volume"]) is int and 0 <= data["volume"] <= 100
                       and type(data["muted"]) is bool and not payload)
            if not speaker:
                raise AssertionError(f"expected info, actual={kind!r} data={data}")


class ReadinessFailure(RuntimeError):
    """Already sanitized, pre-cleanup startup evidence."""


class Host:
    def __init__(self, folder):
        self.folder = Path(folder)
        self.proc = None
        self.staged = None
        self.log = bytearray()
        self.guard = threading.RLock()
        self.tail = bytearray()
        self.truncated = False
        self.deadline = time.monotonic() + 20
        self.reservation = None
        with _allocation_guard(self.deadline):
            self.reserve()
        (self.folder / "state").mkdir()
        (self.folder / "state/config").write_text("even_volume=off\n")
        (self.folder / "addrs").write_text("127.0.0.1\n")

    def reserve(self, port=0):
        self.reservation = socket.socket()
        _inherited.add(self.reservation)
        if port:
            # An explicit protocol restart keeps the advertised address, even
            # while former client connections finish their kernel TIME_WAIT.
            self.reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.reservation.bind(("127.0.0.1", port))
        if port:
            self.reservation.listen()
        self.port = self.reservation.getsockname()[1]

    def release_reservation(self):
        if getattr(self, 'reservation', None) is not None:
            self.reservation.close()
            self.reservation = None

    def logged(self, marker):
        with self.guard:
            while True:
                status = self.proc.poll()
                if status is not None:
                    self.drain_failure()
                    raise self.failure("child-exited", status)
                if marker in self.log:
                    self.log = self.log[self.log.index(marker) + len(marker):]
                    return
                # Retain diagnostics and any split marker, without accumulating
                # an unbounded pre-readiness stream from a noisy failed child.
                keep = max(TAIL, len(marker) - 1)
                if len(self.log) > keep:
                    del self.log[:-keep]
                remaining = self.deadline - time.monotonic()
                if remaining <= 0:
                    self.drain_failure()
                    raise self.failure("deadline", self.proc.poll())
                if select.select([self.proc.stderr], [], [], remaining)[0]:
                    part = os.read(self.proc.stderr.fileno(), 65536)
                    if not part:
                        status = self.proc.poll()
                        reason = "stderr-eof" if status is None else "child-exited"
                        raise self.failure(reason, status)
                    self.record(part)

    def record(self, part):
        self.log.extend(part)
        self.tail.extend(part)
        if len(self.tail) > TAIL:
            del self.tail[:-TAIL]
            self.truncated = True

    def drain_failure(self):
        for _ in range(8):
            if not select.select([self.proc.stderr], [], [], 0)[0]:
                break
            part = os.read(self.proc.stderr.fileno(), 8192)
            if not part:
                break
            self.record(part)

    def failure(self, reason, status=None):
        return ReadinessFailure(
            f"host readiness failed: reason={reason} status={status} "
            f"startup_stderr={safe_tail(self.tail, self.truncated)}")

    def start(self):
        if self.proc is not None:
            raise RuntimeError("host still owned; stop before restart")
        if getattr(self, '_started', False):
            self.deadline = time.monotonic() + 20
        self._started = True
        env = {k: v for k, v in os.environ.items() if not k.startswith("HASSMIC_")}
        env.update(HASSMIC_STATE=str(self.folder / "state"),
                   HASSMIC_PLAY=str(self.folder / "play.raw"),
                   HASSMIC_FAKE_LED=str(self.folder / "led"),
                   HASSMIC_ARB_ADDR="127.255.255.255",
                   HASSMIC_FAKE_ADDRS=str(self.folder / "addrs"),
                   HASSMIC_MDNS_FILE=str(self.folder / "no-mdns"))
        self.log.clear()
        self.tail.clear()
        self.truncated = False
        failure = ["--invalid-test-option"] if "--startup-failure" in sys.argv else []
        try:
            with _allocation_guard(self.deadline):
                if self.reservation is None:
                    self.reserve()
                with LAUNCH_GUARD:
                    binary = self.stage()
                    self.release_reservation()
                    self.launch(
                        [str(binary), "-P", "wyoming", "-D", "-p", str(self.port),
                         "-n", "Wyoming Test", *failure],
                        cwd=binary.parent, env=env, stdout=subprocess.DEVNULL,
                        stderr=subprocess.PIPE, start_new_session=True,
                        preexec_fn=parent_death_guard())
                self.logged(b"wyoming on ")
        except BaseException as error:
            if isinstance(error, Exception) and not isinstance(error, ReadinessFailure):
                reason = f"startup-error errno={getattr(error, 'errno', None)}"
                error = self.failure(reason, self.proc.poll() if self.proc else None)
            try:
                self.stop()
            except Exception:
                # Keep the original startup evidence distinct from cleanup.
                raise self.failure("cleanup-failed") from error
            raise error from None

    def launch(self, *args, **kwargs):
        launched = Future()
        def owner():
            try:
                proc = subprocess.Popen(*args, **kwargs)
            except BaseException as error:
                launched.set_exception(error)
                return
            launched.set_result(proc)
            # Linux PDEATHSIG follows the spawning thread, which must outlive
            # a caller's executor worker. Collection also ends this owner.
            proc.wait()
        threading.Thread(target=owner, daemon=True).start()
        try:
            self.proc = launched.result()
        except BaseException:
            # A handled controller signal must collect an in-flight launch
            # before start's existing failure cleanup can release its stage.
            self.proc = launched.result()
            raise

    def stage(self):
        pinned = Path(subprocess.check_output(
            ["sh", str(ROOT / "scripts/hassmic-host.sh"), "path"], text=True,
            timeout=max(0, self.deadline - time.monotonic())).strip())
        # /tmp is Linux storage under WSL even when TMPDIR points at Windows.
        self.staged = tempfile.TemporaryDirectory(prefix="hassmic-exec-", dir="/tmp")
        folder = Path(self.staged.name).resolve()
        if folder == Path('/mnt') or Path('/mnt') in folder.parents:
            raise self.failure("stage-requires-linux-storage")
        binary = folder / "hassmic-host"
        shutil.copy2(pinned, binary)
        if hashlib.sha256(binary.read_bytes()).digest() != hashlib.sha256(pinned.read_bytes()).digest():
            raise self.failure("stage-hash-mismatch")
        return binary

    def unstage(self):
        staged = getattr(self, 'staged', None)
        if staged is not None:
            staged.cleanup()
            self.staged = None

    def stop(self):
        self.release_reservation()
        if self.proc is None:
            self.unstage()
            return
        proc = self.proc
        deadline = time.monotonic() + 2
        try:
            # A collected child no longer owns its PID (or group number).
            if proc.poll() is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            self.stop_status(proc)
            raise
        finally:
            # Keep failed collection distinguishable from a stopped host, but
            # release the pipe even when signaling or waiting fails.
            proc.stderr.close()
            self.unstage()
        self.proc = None

    def stop_status(self, proc):
        # Preserve kernel evidence of a missed reap without reading command
        # lines, environment, paths, or any unbounded stream from the child.
        print(f"host reap timeout: pid={proc.pid} returncode={proc.returncode}",
              file=sys.stderr, flush=True)
        with contextlib.suppress(OSError):
            print(f"pgid={os.getpgid(proc.pid)} sid={os.getsid(proc.pid)}",
                  file=sys.stderr, flush=True)
            for task in Path(f"/proc/{proc.pid}/task").iterdir():
                status = (task / "status").read_text().splitlines()
                fields = [line for line in status if line.split(':')[0] in
                          ('State', 'Pid', 'PPid', 'SigPnd', 'ShdPnd')]
                print(f"task={task.name} wchan={(task / 'wchan').read_text()} "
                      + ' '.join(fields), file=sys.stderr, flush=True)

    def command(self, command):
        if command == "size":
            play = self.folder / "play.raw"
            print(play.stat().st_size if play.exists() else 0, flush=True)
            return
        if command == "wake":
            self.logged(b"satellite: running")
            self.proc.send_signal(signal.SIGUSR1)
        elif command == "stop":
            self.stop()
        elif command == "restart":
            if self.proc is not None:
                raise RuntimeError("host still owned; stop before restart")
            self.deadline = time.monotonic() + 20
            with _allocation_guard(self.deadline):
                self.reserve(self.port)
            self._started = False
            self.start()
        else:
            raise ValueError("invalid controller command")
        print(command, flush=True)


@controller_signals()
def main():
    failed = False
    with tempfile.TemporaryDirectory(prefix="wyoming-t3-") as folder:
        host = Host(folder)
        try:
            host.start()
            print(host.port, flush=True)
            for command in sys.stdin:
                host.command(command.strip())
        except RuntimeError:
            if "--startup-failure" not in sys.argv:
                raise
            failed = True
        finally:
            host.stop()
    if "--startup-failure" in sys.argv:
        assert failed, "invalid host option unexpectedly started"
        print("startup-failed-cleaned", flush=True)
        return
    print("cleaned", flush=True)


if __name__ == "__main__":
    main()
