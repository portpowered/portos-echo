"""T3-only device supervisor; never invokes adb, port or a remote endpoint."""
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys


def main():
    if os.name == "nt":
        path = subprocess.check_output(
            ["wsl", "-d", "Ubuntu", "--exec", "wslpath", "-u", str(Path(__file__).resolve())],
            text=True).strip()
        return subprocess.call(["wsl", "-d", "Ubuntu", "--exec", "python3", "-u", path])
    source = Path(__file__).resolve().parents[2] / "src/lab_run/sim/host.py"
    spec = importlib.util.spec_from_file_location("native_host", source)
    native = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(native)
    original = native.Host

    class Supervisor(original):
        def command(self, value):
            if value.get("action") == "say" and self.muted:
                # Playback in a muted room cannot trigger Pryon. SIGUSR1 is
                # a forced wake in hassmic-host, retained until unmute by the
                # current pin, so only feed the utterance in this window.
                import fcntl
                data = bytes(value["pcm"])
                if not data or len(data) > 4096 or len(data) % 2:
                    raise ValueError("invalid fixture")
                with self.condition:
                    self.fixture = data
                    target = self.fed + fcntl.fcntl(self.fd, fcntl.F_GETPIPE_SZ) + 8192
                    ready = self.condition.wait_for(
                        lambda: self.fed >= target or self.error or self.cancelled.is_set(), 20)
                    if not ready or self.error:
                        raise RuntimeError("fixture consumption failed")
                    if self.cancelled.is_set():
                        raise native.HostCancelled("host command cancelled")
                native.emit({"event": "host.ack"})
                return
            if value.get("action") == "kill":
                if value.get("pid") != self.proc.pid:
                    raise ValueError("wrong target")
                os.kill(self.proc.pid, signal.SIGKILL)
                self.proc.wait(timeout=3)
                # Existing owner collects FIFO/log threads and relaunches on
                # the same local port. No fabricated bridge/UI observations.
                return super().command({"action": "restart"})
            return super().command(value)

    native.Host = Supervisor
    return native.main()


if __name__ == "__main__":
    sys.exit(main())
