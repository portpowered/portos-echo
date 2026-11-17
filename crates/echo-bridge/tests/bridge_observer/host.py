"""Failure-only sideband snapshot; the existing controller still owns stdout.

The base host owns readiness. Only a diagnostic request reads additional
stderr, under the same lock. EOF retains controller cleanup.
"""
import collections
import json
import os
from pathlib import Path
import select
import socket
import sys
import threading

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tests"))
import bridge_host


class ObservedHost(bridge_host.Host):
    def __init__(self, folder):
        super().__init__(folder)
        self.observed = collections.deque(maxlen=8192)
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        if self.listener.getsockname()[1] == self.port:
            replacement = socket.socket()
            replacement.bind(("127.0.0.1", 0))
            self.listener.close()
            self.listener = replacement
        self.listener.listen()
        print(f"observer port: {self.listener.getsockname()[1]}", file=sys.stderr, flush=True)
        threading.Thread(target=self.snapshots, daemon=True).start()

    def record(self, part):
        self.observed.extend(part)
        super().record(part)

    def snapshots(self):
        while True:
            try:
                connection, _ = self.listener.accept()
            except OSError:
                return
            with connection:
                connection.settimeout(1)
                state = self.snapshot()
                try:
                    connection.sendall(json.dumps(state).encode())
                except OSError:
                    pass

    def snapshot(self):
        if not self.guard.acquire(blocking=False):
            return {"state": "unavailable: controller reading host log"}
        try:
            process = self.proc
            if process is not None and not process.stderr.closed:
                for _ in range(8):
                    if not select.select([process.stderr], [], [], 0)[0]:
                        break
                    part = os.read(process.stderr.fileno(), 8192)
                    if not part:
                        break
                    self.record(part)
            return {"port": self.port,
                    "process": "absent" if process is None else process.poll(),
                    "log": bytes(self.observed).decode(errors="replace")}
        finally:
            self.guard.release()

    def stop(self):
        with self.guard:
            super().stop()
        # stop/restart is part of the existing controller protocol, so the
        # sideband listener survives until the controller itself exits.


bridge_host.Host = ObservedHost
if __name__ == "__main__":
    bridge_host.main()
