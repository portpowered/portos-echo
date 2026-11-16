"""Private pinned-host owner. JSON stdin, bounded fixed replies, EOF teardown.

Linux/WSL only below the launcher. The upstream audio-file backend has a real
clock; FIFO consumption and log markers provide readiness without sleep oracles.
"""
import contextlib
import json
import os
from pathlib import Path
import queue
import select
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[5]
LIMIT = 65536


def emit(value):
    print(json.dumps(value), flush=True)


class LauncherError(RuntimeError):
    """Only fixed, sanitized launcher-stage diagnostics belong here."""


class HostCancelled(RuntimeError):
    """Caller EOF interrupts work; the cleanup result still comes from close."""


class LauncherChild:
    """Keep the handle until collection, including setup/forwarding failures.

    Four seconds includes the guest Host's three-second group grace and stays
    inside the Rust owner's five seconds. Closing stdin requests guest cleanup;
    terminating a wsl client alone is never evidence of guest collection.
    """
    def __init__(self, proc):
        self.proc = proc
        self.deadline = None
        self.status = None
        self.forced = False
        self.failure = None

    def exited(self):
        try:
            return self.proc.poll() is not None
        except OSError:
            raise LauncherError("cleanup: cannot observe launcher child") from None

    def close(self):
        if self.status is not None:
            if self.failure:
                raise LauncherError(self.failure)
            return self.status
        if self.deadline is None:
            self.deadline = time.monotonic() + 4
            try:
                self.proc.stdin.close()
            except (OSError, ValueError):
                self.failure = "cleanup: cannot close launcher input"
        try:
            self.status = self.proc.wait(timeout=max(0, self.deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            if not self.forced:
                try:
                    self.proc.kill()
                except OSError:
                    raise LauncherError("cleanup: cannot force launcher child") from None
                self.forced = True
            try:
                self.status = self.proc.wait()
            except OSError:
                raise LauncherError("cleanup: cannot collect launcher child") from None
        except OSError:
            raise LauncherError("cleanup: cannot collect launcher child") from None
        if self.failure:
            raise LauncherError(self.failure)
        return self.status


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
    try:
        owner = LauncherChild(subprocess.Popen(
            ["wsl", "-d", "Ubuntu", "--exec", "wslpath", "-u", str(Path(__file__).resolve())],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL))
        owners.append(owner)
        reader = threading.Thread(target=launcher_translation, args=(owner.proc, events), daemon=True)
        reader.start()
        threading.Thread(target=launcher_input, args=(events,), daemon=True).start()
        return launcher_forward(events, owner, owners)
    except LauncherError as error:
        sys.stderr.write(str(error) + "\n")
        emit({"event": "host.failed"})
        return 1
    except (OSError, ValueError, RuntimeError, queue.Full):
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
            emit({"event": "host.failed"})
            return 1


def launcher_forward(events, translation, owners):
    owner = translation
    pending = []
    commands = queue.Queue(maxsize=16)
    writer = None
    translated = False
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
                    ["wsl", "-d", "Ubuntu", "--exec", "python3", "-u", value],
                    stdin=subprocess.PIPE, stderr=subprocess.DEVNULL, bufsize=0))
                owners.append(owner)
                writer = threading.Thread(target=launcher_write, args=(owner.proc, commands, events), daemon=True)
                writer.start()
                translated = True
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


class Host:
    def __init__(self, folder):
        self.folder = Path(folder)
        self.proc = None
        self.stop = threading.Event()
        self.cancelled = threading.Event()
        self.condition = threading.Condition()
        self.log = bytearray()
        self.fixture = bytes(640)
        self.fed = 0
        self.fd = None
        self.threads = []
        self.error = False
        self.closed = False

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
            if not select.select([], [self.fd], [], 0.1)[1]:
                continue
            with self.condition:
                data = self.fixture
            try:
                count = os.write(self.fd, data)
            except BlockingIOError:
                continue
            with self.condition:
                self.fed += count
                self.condition.notify_all()

    def start(self):
        (self.folder / "state").mkdir()
        (self.folder / "state/config").write_text("even_volume=off\n")
        (self.folder / "addrs").write_text("127.0.0.1\n")
        os.mkfifo(self.folder / "capture")
        self.fd = os.open(self.folder / "capture", os.O_RDWR | os.O_NONBLOCK)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("HASSMIC_")}
        env.update(HASSMIC_STATE=str(self.folder / "state"), HASSMIC_CAP=str(self.folder / "capture"),
                   HASSMIC_PLAY=str(self.folder / "play.raw"), HASSMIC_FAKE_LED=str(self.folder / "led"),
                   HASSMIC_ARB_ADDR="127.255.255.255", HASSMIC_ARB_RIVALS="0",
                   HASSMIC_FAKE_ADDRS=str(self.folder / "addrs"), HASSMIC_MDNS_FILE=str(self.folder / "no-mdns"))
        self.proc = subprocess.Popen(
            ["sh", str(ROOT / "scripts/hassmic-host.sh"), "run", "-P", "wyoming", "-D", "-p", str(port),
             "-n", "Lab Sim " + self.folder.name], cwd=self.folder, env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True)
        for target in [self.logs, self.feed]:
            thread = threading.Thread(target=target, daemon=True)
            thread.start()
            self.threads.append(thread)
        self.logged(b"wyoming on ")
        emit({"event": "host.ready", "address": "127.0.0.1:" + str(port)})

    def command(self, value):
        if self.cancelled.is_set():
            if self.error:
                raise RuntimeError("host input failed")
            raise HostCancelled("host command cancelled")
        action = value.get("action")
        if action == "registered":
            self.logged(b"satellite: running")
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
        host = Host(folder)
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
    sys.exit(launch_windows() if os.name == "nt" else main())
