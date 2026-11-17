"""Private T3 process identities captured while the direct bridge is registered."""
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import select
import signal


def windows_processes():
    class Entry(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD),
                    ("pid", wintypes.DWORD), ("heap", ctypes.c_size_t),
                    ("module", wintypes.DWORD), ("threads", wintypes.DWORD),
                    ("parent", wintypes.DWORD), ("priority", wintypes.LONG),
                    ("flags", wintypes.DWORD), ("name", wintypes.WCHAR * 260)]
    api = ctypes.WinDLL("kernel32", use_last_error=True)
    api.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    api.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    api.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    api.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    api.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot = api.CreateToolhelp32Snapshot(2, 0)
    if snapshot == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    entries = {}
    try:
        entry = Entry()
        entry.size = ctypes.sizeof(entry)
        more = api.Process32FirstW(snapshot, ctypes.byref(entry))
        while more:
            entries[entry.pid] = (entry.parent, entry.name.lower())
            more = api.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        api.CloseHandle(snapshot)
    return entries


def processes():
    if os.name == "nt":
        return windows_processes()
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
        if os.name != "nt":
            self.handle = os.pidfd_open(pid)
            return
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.api.OpenProcess.restype = wintypes.HANDLE
        self.api.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        self.api.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        self.api.CloseHandle.argtypes = [wintypes.HANDLE]
        self.handle = self.api.OpenProcess(0x100001, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())

    def completed(self):
        if os.name != "nt":
            return bool(select.select([self.handle], [], [], 0)[0])
        status = self.api.WaitForSingleObject(self.handle, 0)
        if status not in (0, 258):
            raise ctypes.WinError(ctypes.get_last_error())
        return status == 0

    def close(self):
        # Preserve the pre-cleanup observation; rescue must never turn a leak green.
        completed = self.completed()
        if os.name != "nt":
            if not completed:
                signal.pidfd_send_signal(self.handle, signal.SIGKILL)
            os.close(self.handle)
        else:
            if not completed:
                self.api.TerminateProcess(self.handle, 1)
            self.api.CloseHandle(self.handle)
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
            if name in ("echo-lab", "echo-lab.exe"):
                break
            parent = ancestor
        else:
            raise RuntimeError("CLI owner not found")
        children = [pid for pid, (ancestor, name) in entries.items()
                    if ancestor == parent and name in ("echo-bridge", "echo-bridge.exe")]
        if len(children) != 1:
            raise RuntimeError("registered direct bridge owner not found")
        self.owners.append(Owner(children[0]))

    def close(self):
        if self.audit:
            Path(self.audit).write_text(json.dumps([owner.close() for owner in self.owners]))
