"""Private pinned-host owner. JSON stdin, bounded fixed replies, EOF teardown.

Linux/WSL only below the launcher. The upstream audio-file backend has a real
clock; FIFO consumption and log markers provide readiness without sleep oracles.
"""
import contextlib
import json
import os
from pathlib import Path
import queue
import runpy
import select
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

finite_input = runpy.run_path(str(Path(__file__).with_name("m3") / "host_input.py"))["finite_input"]

ROOT = Path(__file__).resolve().parents[5]
LIMIT = 65536


def emit(value):
    print(json.dumps(value), flush=True)


sys.path.insert(0, str(ROOT / "scripts"))
from wsl_distro import resolve_wsl_distro
from wsl_host import LauncherError, LauncherChild, HostCancelled

# Both T3 launchers share the native transport; the Linux Host stays direct.
from wsl_relay import observations, guest


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


def launcher_translation(proc, events):
    try:
        data = proc.stdout.read(LIMIT + 1)
        if len(data) > LIMIT or proc.wait() != 0:
            raise ValueError("translation failed")
        path = data.decode("utf-8").strip()
        if not path or "\n" in path:
            raise ValueError("invalid translated path")
        events.put(("translated", path))
    except (OSError, ValueError, UnicodeError):
        events.put(("error", None))


def launcher_write(proc, commands, events):
    # Raw pipes avoid a buffered-writer lock in the cleanup owner. A blocked
    # write must not prevent the supervisor from observing caller EOF.
    try:
        while True:
            data = commands.get()
            if data is None:
                proc.stdin.close()
                return
            while data:
                count = proc.stdin.write(data)
                if not count:
                    raise OSError("launcher write failed")
                data = data[count:]
    except (OSError, ValueError):
        events.put(("error", None))


def launch_windows():
    events = queue.Queue()
    owner = None
    owners = []
    reader = None
    distro = None
    try:
        distro = resolve_wsl_distro()
        owner = LauncherChild(subprocess.Popen(
            ["wsl", "-d", distro, "--exec", "wslpath", "-u", str(Path(__file__).resolve())],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL))
        owners.append(owner)
        reader = threading.Thread(target=launcher_translation, args=(owner.proc, events), daemon=True)
        reader.start()
        threading.Thread(target=launcher_input, args=(events,), daemon=True).start()
        status = launcher_forward(events, owner, owners, distro)
        if status:
            sys.stderr.write(f"WSL distro '{distro}': operation failed\n")
        return status
    except LauncherError as error:
        sys.stderr.write(f"WSL distro '{distro}': {error}\n")
        emit({"event": "host.failed"})
        return 1
    except (OSError, ValueError, RuntimeError, queue.Full) as error:
        sys.stderr.write((f"WSL distro '{distro}': operation failed" if distro else str(error)) + "\n")
        emit({"event": "host.failed"})
        return 1
    finally:
        cleanup_failed = False
        for acquired in reversed(owners):
            try:
                acquired.close()
            except (OSError, ValueError, LauncherError):
                sys.stderr.write("cleanup: cannot collect launcher child\n")
                cleanup_failed = True
        if reader and reader.ident is not None:
            reader.join(timeout=1)
            if reader.is_alive():
                sys.stderr.write("cleanup: launcher reader did not stop\n")
                cleanup_failed = True
        if owner and owner.proc.stdout and (reader is None or not reader.is_alive()):
            try:
                owner.proc.stdout.close()
            except (OSError, ValueError):
                sys.stderr.write("cleanup: cannot close launcher output\n")
                cleanup_failed = True
        if cleanup_failed:
            sys.stderr.write(f"WSL distro '{distro}': cleanup failed\n")
            emit({"event": "host.failed"})
            return 1


def launcher_emit(value):
    sys.stdout.buffer.write((json.dumps(value) + "\n").encode("utf-8"))
    sys.stdout.buffer.flush()


def launcher_forward(events, translation, owners, distro=None):
    owner = translation
    pending = []
    commands = queue.Queue(maxsize=16)
    writer = None
    translated = False
    output = None
    relays = []
    startup = time.monotonic() + 20
    try:
        while True:
            if translated and owner.exited():
                return owner.close()
            if not translated and time.monotonic() >= startup:
                raise RuntimeError("launch: translation timed out")
            try:
                action, value = events.get(timeout=0.01)
            except queue.Empty:
                continue
            if action == "eof":
                if writer:
                    # Retain one deadline while the writer drains commands and
                    # closes the pipe. A stalled writer is forced at that same
                    # deadline, rather than blocking EOF detection or cleanup.
                    commands.put_nowait(None)
                    owner.deadline = time.monotonic() + 4
                status = owner.close()
                return status if translated else 1
            if action == "error":
                raise RuntimeError("launch: launcher input or translation failed")
            if action == "translated":
                if translation.close() != 0:
                    return 1
                owner = LauncherChild(subprocess.Popen(
                    ["wsl", "-d", distro or resolve_wsl_distro(), "--exec", "python3", "-u", value,
                     *(["--finite"] if "--finite" in sys.argv[1:] else [])],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0))
                owners.append(owner)
                translated = True
                output = threading.Thread(target=observations, args=(
                    owner.proc, ["wsl", "-d", distro or resolve_wsl_distro(), "--exec",
                                 "python3", "-u", value, "--relay"], relays, launcher_emit, events))
                output.start()
                writer = threading.Thread(target=launcher_write, args=(owner.proc, commands, events), daemon=True)
                writer.start()
                for line in pending:
                    commands.put_nowait(line)
                pending.clear()
            elif translated:
                commands.put_nowait(value)
            elif sum(map(len, pending)) + len(value) <= LIMIT:
                pending.append(value)
            else:
                raise ValueError("launch: startup input exceeded limit")
    finally:
        try:
            owner.close()
        finally:
            if writer and writer.ident is not None:
                with contextlib.suppress(queue.Full):
                    commands.put_nowait(None)
                writer.join(timeout=0.5)
            failed = False
            if output and output.ident is not None:
                output.join(timeout=0.5)
                failed = output.is_alive()
            if translated and not failed:
                owner.proc.stdout.close()
            for relay in relays:
                try:
                    relay.close()
                except (OSError, ValueError, RuntimeError):
                    failed = True
            if failed:
                raise RuntimeError("launch: output cleanup failed")


class Host:
    def __init__(self, folder, finite_mode=False):
        self.folder = Path(folder)
        self.finite_mode = finite_mode
        self.proc = None
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
        if self.finite_mode:
            import fcntl
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
        self.proc = subprocess.Popen(
            ["sh", str(ROOT / "scripts/hassmic-host.sh"), "run", "-P", "wyoming", "-D", "-p", str(port),
             "-b", str(self.folder / "keys"), "-n", "Lab Sim " + self.folder.name], cwd=self.folder, env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True)
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
            with (self.folder / "play.raw").open("rb") as recording:
                data = recording.read(LIMIT + 1)
            if len(data) > LIMIT or len(data) % 2:
                raise ValueError("invalid playback observation")
            emit({"event": "host.ack", "pcm_bytes": len(data),
                  "nonzero_bytes": sum(byte != 0 for byte in data)})
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
        deadline = time.monotonic() + 4
        try:
            if self.proc and self.proc.poll() is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.proc.pid, signal.SIGTERM)
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
            if failed:
                raise RuntimeError("host thread cleanup failed")
        self.closed = True


def host_input(host, events):
    # Read EOF independently of log/FIFO waits. Only the main owner closes the
    # host: interrupt its wait, then let its finally stop and collect the group
    # before the Windows client's retained four-second deadline expires.
    clean = False
    try:
        clean = launcher_input(events)
    finally:
        with host.condition:
            host.error = host.error or not clean
            host.cancelled.set()
            host.condition.notify_all()


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
    if len(sys.argv) == 3 and sys.argv[1] == "--relay":
        guest(int(sys.argv[2]))
    else:
        sys.exit(launch_windows() if os.name == "nt" else main())
