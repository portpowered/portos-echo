"""T3 controller for one prebuilt pinned host, never a hardware device.

stdin commands own the lifecycle; EOF (including a Rust panic) reaps the group.
Only fixed status lines cross stdout. Temporary synthetic audio is deleted.
"""
import contextlib
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


class Host:
    def __init__(self, folder):
        self.folder = Path(folder)
        self.proc = None
        self.log = bytearray()
        self.deadline = time.monotonic() + 20
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        (self.folder / "state").mkdir()
        (self.folder / "state/config").write_text("even_volume=off\n")
        (self.folder / "addrs").write_text("127.0.0.1\n")

    def logged(self, marker):
        while marker not in self.log:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or self.proc.poll() is not None:
                raise RuntimeError("host readiness failed")
            if select.select([self.proc.stderr], [], [], remaining)[0]:
                self.log.extend(os.read(self.proc.stderr.fileno(), 65536))
        self.log = self.log[self.log.index(marker) + len(marker):]

    def start(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("HASSMIC_")}
        env.update(HASSMIC_STATE=str(self.folder / "state"),
                   HASSMIC_PLAY=str(self.folder / "play.raw"),
                   HASSMIC_FAKE_LED=str(self.folder / "led"),
                   HASSMIC_ARB_ADDR="127.255.255.255",
                   HASSMIC_FAKE_ADDRS=str(self.folder / "addrs"),
                   HASSMIC_MDNS_FILE=str(self.folder / "no-mdns"))
        self.log.clear()
        failure = ["--invalid-test-option"] if "--startup-failure" in sys.argv else []
        self.proc = subprocess.Popen(
            ["sh", str(ROOT / "scripts/hassmic-host.sh"), "run", "-P", "wyoming",
             "-D", "-p", str(self.port), "-n", "Wyoming Test", *failure],
            cwd=self.folder, env=env, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, start_new_session=True)
        self.logged(b"wyoming on ")

    def stop(self):
        if self.proc is None:
            return
        with contextlib.suppress(ProcessLookupError):
            os.killpg(self.proc.pid, signal.SIGKILL)
        self.proc.wait(timeout=2)
        self.proc.stderr.close()
        self.proc = None

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
            self.start()
        else:
            raise ValueError("invalid controller command")
        print(command, flush=True)


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
