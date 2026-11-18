"""T3 observers: retained identities and whole-stage scans, never global kills."""
import contextlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[6]
SIM = ROOT / "crates/echo-lab/src/lab_run/sim/host.py"
CAPTURE = ROOT / "scripts/capture-wyoming.py"
CAPTURE_RUNNER = r'''import os, runpy, socket, sys, threading
from unittest import mock
original = socket.SocketIO.readinto
held = False
def read(self, buffer):
    global held
    count = original(self, buffer)
    if not held and b'Capture Echo' in bytes(buffer[:count]):
        held = True
        print('capture-ready', flush=True)
        threading.Event().wait()
    return count
sys.argv = sys.argv[1:]
with mock.patch('socket.SocketIO.readinto', read):
    runpy.run_path(sys.argv[0], run_name='__main__')
'''

def identity(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        return stat[19], stat[0]
    except (OSError, IndexError):
        return None

def live(pid, start):
    current = identity(pid)
    return current is not None and current[0] == start and current[1] != "Z"

def stage_pids(stage):
    found = {}
    if stage.parent != Path("/tmp") or not stage.name.startswith("hassmic-exec-"):
        return found
    for entry in Path('/proc').iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            executable = os.readlink(entry / 'exe').removesuffix(' (deleted)')
            if Path(executable).parent == stage:
                current = identity(int(entry.name))
                if current:
                    found[int(entry.name)] = current[0]
        except OSError:
            pass
    return found

class Controller:
    def __init__(self, kind, folder):
        self.kind = kind
        self.stage = None
        self.owned = {}
        self.pidfds = []
        self.output = b""
        self.destination = folder / 'output'
        self.destination.mkdir()
        (self.destination / 'previous').write_bytes(b'prior bytes')
        if kind == 'test':
            command = [sys.executable, '-u', str(ROOT / 'tests/wyoming_host.py')]
        elif kind == 'sim':
            command = [sys.executable, '-u', str(SIM)]
        else:
            command = [sys.executable, '-u', '-c', CAPTURE_RUNNER, str(CAPTURE),
                       '--output-dir', str(self.destination)]
        self.proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, start_new_session=True)
        self.port = None

    def line(self):
        deadline = time.monotonic() + 20
        while b"\n" not in self.output:
            if not select.select([self.proc.stdout], [], [], max(0, deadline-time.monotonic()))[0]:
                raise AssertionError('controller readiness deadline')
            chunk = os.read(self.proc.stdout.fileno(), 65536)
            if not chunk:
                raise AssertionError(f'controller exited: {self.proc.stderr.read().decode()}')
            self.output += chunk
        line, self.output = self.output.split(b"\n", 1)
        return line.decode()

    def ready(self):
        if self.kind == 'test':
            self.port = int(self.line())
        elif self.kind == 'sim':
            value = json.loads(self.line())
            assert value['event'] == 'host.ready', value
            self.port = int(value['address'].rsplit(':', 1)[1])
        else:
            while self.line() != 'capture-ready':
                pass
        self.identify()

    def identify(self):
        # Linux records children under their spawning task, including retained
        # owner threads. Keep attribution to this controller before stage scans.
        children = set()
        for task in Path(f'/proc/{self.proc.pid}/task').iterdir():
            with contextlib.suppress(FileNotFoundError):
                children.update((task / 'children').read_text().split())
        for child in children:
            try:
                path = Path(os.readlink(f'/proc/{child}/exe'))
                if path.name == 'hassmic-host':
                    self.stage = path.parent
                    self.owned[int(child)] = identity(int(child))[0]
            except OSError:
                pass
        assert self.stage is not None, 'ready controller has no identifiable host'
        self.owned.update(stage_pids(self.stage))
        for pid, start in self.owned.items():
            if live(pid, start):
                with contextlib.suppress(ProcessLookupError):
                    self.pidfds.append(os.pidfd_open(pid))

    def describe(self):
        with socket.create_connection(('127.0.0.1', self.port), timeout=2) as conn:
            conn.sendall(b'{"type":"describe"}\n')
            with conn.makefile('rb') as stream:
                head = json.loads(stream.readline())
                assert head['type'] == 'info', head
                if head.get('data_length'):
                    json.loads(stream.read(head['data_length']))

    def observe_death(self, sig):
        started = time.monotonic()
        self.proc.send_signal(sig)
        deadline = started + 2
        while True:
            self.owned.update(stage_pids(self.stage))
            survivors = [pid for pid, start in self.owned.items() if live(pid, start)]
            removed = not self.stage.exists()
            if not survivors and (sig == signal.SIGKILL or removed):
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            # Bounded rescan interval; success always depends on kernel state.
            select.select([], [], [], min(remaining, .01))
        elapsed = time.monotonic() - started
        snapshot = subprocess.run(['ps', '-o', 'pid,ppid,pgid,sid,stat,args', '-p',
                                   ','.join(map(str, self.owned))], capture_output=True, text=True)
        print(f'{self.kind} {signal.Signals(sig).name}: elapsed={elapsed:.4f} '
              f'survivor_count={len(survivors)} stage_removed={removed}', flush=True)
        print(snapshot.stdout, end='', flush=True)
        assert not survivors, f'live owned processes before rescue: {survivors}'
        assert elapsed <= 2, f"death observation exceeded 2 s: {elapsed}"
        if sig != signal.SIGKILL:
            assert removed, 'handled exit retained stage'
            assert self.proc.wait(timeout=2) != 0
        assert self.stage.parent == Path('/tmp') and self.stage.name.startswith('hassmic-exec-'), \
            'controller did not privately stage its executable'
        if self.kind == 'capture':
            assert (self.destination / 'previous').read_bytes() == b'prior bytes'
            assert list(self.destination.iterdir()) == [self.destination / 'previous']
            if sig != signal.SIGKILL:
                assert not self.destination.with_name('output.lock').exists()
                assert b'capture cancelled by signal' in self.proc.stderr.read()

    def close(self):
        # Called only after the observation/assertion, including failing base runs.
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=3)
        for pid, start in self.owned.items():
            if live(pid, start):
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGKILL)
        for stream in (self.proc.stdin, self.proc.stdout, self.proc.stderr):
            stream.close()
        for fd in self.pidfds:
            os.close(fd)
        if self.stage and self.stage.parent == Path('/tmp') and self.stage.name.startswith('hassmic-exec-'):
            shutil.rmtree(self.stage, ignore_errors=True)
