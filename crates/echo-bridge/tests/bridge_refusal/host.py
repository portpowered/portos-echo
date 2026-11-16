"""T3 pinned-host log observer; one monotonic clock includes all dispatch delay."""
import json
from pathlib import Path
import signal
import sys
import tempfile
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tests"))
from wyoming_host import Host


class Observations:
    def __init__(self, host):
        self.host = host
        self.rows = []
        self.condition = threading.Condition()
        self.start = 0
        self.injected = 0
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        for raw in self.host.proc.stderr:
            with self.condition:
                self.rows.append((time.monotonic(), raw.decode(errors="replace").strip().split("Z ", 1)[-1]))
                self.condition.notify_all()

    def wait(self, predicate):
        with self.condition:
            if not self.condition.wait_for(predicate, timeout=3):
                raise AssertionError(f"missing host transition: {self.rows[-20:]}")

    def wake(self):
        self.wait(lambda: any("satellite: running" in row for _, row in self.rows))
        with self.condition:
            self.start = len(self.rows)
            self.injected = time.monotonic()
        self.host.proc.send_signal(signal.SIGUSR1)
        self.wait(lambda: any("-> listening" in row for _, row in self.rows[self.start:]))

    def result(self, failure):
        def complete():
            rows = self.rows[self.start:]
            return any("-> idle" in row for _, row in rows) and (
                not failure or any(row.startswith("server error: ") for _, row in rows))

        self.wait(complete)
        with self.condition:
            rows = self.rows[self.start:]
            entered = next(stamp for stamp, row in rows if "-> listening" in row)
            idle = next(stamp for stamp, row in rows if "-> idle" in row)
            errors = [json.loads(row.removeprefix("server error: "))
                      for _, row in rows if row.startswith("server error: ")]
            pcm = self.host.folder / "play.raw"
            result = {
                # Injection precedes the refusal, so this is a conservative upper bound.
                "refusal_to_idle_ms": max(0, (idle - self.injected) * 1000),
                "listening_ms": max(0, (idle - entered) * 1000),
                "errors": errors,
                "total_errors": sum(row.startswith("server error: ") for _, row in self.rows),
                "connections": sum(row == "client connected" for _, row in self.rows),
                "disconnections": sum(row == "client disconnected" for _, row in self.rows),
                "pcm_bytes": pcm.stat().st_size if pcm.exists() else 0,
            }
        print(json.dumps(result), flush=True)


def main():
    with tempfile.TemporaryDirectory(prefix="refusal-t3-") as folder:
        host = Host(folder)
        try:
            host.start()
            observed = Observations(host)
            print(host.port, flush=True)
            for command in sys.stdin:
                command = command.strip()
                if command == "failing":
                    # Acknowledge before the caller injects the Live error. Keep
                    # dispatch/transport delay in this conservative upper bound.
                    observed.injected = time.monotonic()
                    print("armed", flush=True)
                    continue
                if command in ("wake", "start"):
                    observed.wake()
                if command in ("wake", "result", "idle"):
                    observed.result(command != "idle")
                else:
                    assert command == "start"
                    print("listening", flush=True)
        finally:
            host.stop()
    print("cleaned", flush=True)


if __name__ == "__main__":
    main()
