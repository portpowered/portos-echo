"""Private T3 process identities captured while the direct bridge is registered."""
import json
import os
from pathlib import Path
import select
import signal


def processes():
    entries = {}
    for directory in Path("/proc").iterdir():
        if directory.name.isdigit():
            try:
                stat = (directory / "stat").read_text().rsplit(")", 1)[1].split()
                entries[int(directory.name)] = (int(stat[1]), (directory / "exe").resolve().name)
            except (OSError, ValueError):
                pass  # Unrelated processes may exit during enumeration.
    return entries


class Owner:
    def __init__(self, pid):
        self.pid = pid
        self.handle = os.pidfd_open(pid)

    def completed(self):
        return bool(select.select([self.handle], [], [], 0)[0])

    def close(self):
        # Preserve the pre-cleanup observation; rescue must never turn a leak green.
        completed = self.completed()
        if not completed:
            signal.pidfd_send_signal(self.handle, signal.SIGKILL)
        os.close(self.handle)
        return {"owner_completed": completed, "pid": self.pid}


class Runtime:
    def __init__(self):
        self.audit = os.environ.get("ECHO_LAB_TEST_RUNTIME_OWNERS")
        self.owners = []

    def registered(self):
        if not self.audit:
            return
        entries = processes()
        parent = os.getpid()
        visited = set()
        while parent not in visited:
            visited.add(parent)
            ancestor, name = entries[parent]
            if name == "echo-lab":
                break
            parent = ancestor
        else:
            raise RuntimeError("CLI owner not found")
        children = [pid for pid, (ancestor, name) in entries.items()
                    if ancestor == parent and name == "echo-bridge"]
        if len(children) != 1:
            raise RuntimeError("registered direct bridge owner not found")
        self.owners.append(Owner(children[0]))

    def close(self):
        if self.audit:
            Path(self.audit).write_text(json.dumps([owner.close() for owner in self.owners]))
