"""Private pinned-host owner. JSON stdin, bounded fixed replies, EOF teardown.

Linux/WSL only below the launcher. The upstream audio-file backend has a real
clock; FIFO consumption and log markers provide readiness without sleep oracles.
"""
import contextlib
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

ROOT = Path(__file__).resolve().parents[5]
LIMIT = 65536


def emit(value):
    print(json.dumps(value), flush=True)


def launch_windows():
    translated = subprocess.run(
        ["wsl", "-d", "Ubuntu", "--exec", "wslpath", "-u", str(Path(__file__).resolve())],
        capture_output=True, check=True, text=True).stdout.strip()
    return subprocess.call(["wsl", "-d", "Ubuntu", "--exec", "python3", "-u", translated])


class Host:
    def __init__(self, folder):
        self.folder = Path(folder)
        self.proc = None
        self.stop = threading.Event()
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
            ready = self.condition.wait_for(lambda: marker in self.log or self.error, 20)
            if not ready or self.error:
                raise RuntimeError("host readiness failed")
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
                    if not self.condition.wait_for(lambda: self.fed >= target or self.error, 20) or self.error:
                        raise RuntimeError("fixture consumption failed")
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


def main():
    failed = False
    with tempfile.TemporaryDirectory(prefix="echo-lab-sim-") as folder:
        host = Host(folder)
        try:
            host.start()
            while True:
                line = sys.stdin.buffer.readline(LIMIT + 1)
                if not line:
                    break
                if len(line) > LIMIT or not line.endswith(b"\n"):
                    raise ValueError("oversized command")
                host.command(json.loads(line))
        except (OSError, ValueError, KeyError, RuntimeError):
            failed = True
            emit({"event": "host.failed"})
        finally:
            host.close()
    emit({"event": "host.closed"})
    return int(failed)


if __name__ == "__main__":
    sys.exit(launch_windows() if os.name == "nt" else main())
