"""Private numeric-controller adapter; lifecycle remains owned by guest stdin."""
import argparse
import contextlib
from pathlib import Path
import queue
import signal
import subprocess
import sys
import threading

from wsl_distro import resolve_wsl_distro
from wsl_relay import Relay


def lines(stream, name, events):
    try:
        for line in stream:
            if len(line) > 65536:
                raise ValueError('controller line too long')
            events.put((name, line))
        events.put((name, None))
    except (OSError, ValueError):
        events.put(('error', None))


def collect(proc, relays, readers):
    """Attempt every acquired owner even if an earlier cleanup fails."""
    failed = False
    with contextlib.suppress(OSError, ValueError):
        proc.stdin.close()
    try:
        proc.wait(timeout=4)
    except (OSError, subprocess.TimeoutExpired):
        try:
            proc.kill()
            proc.wait()
        except OSError:
            pass
        failed = True
    for relay in relays.values():
        try:
            relay.close()
        except (OSError, ValueError, RuntimeError):
            failed = True
    for reader in readers:
        if reader.ident is not None:
            reader.join(timeout=0.5)
        failed = reader.is_alive() or failed
    for stream in (proc.stdout, proc.stderr):
        try:
            stream.close()
        except (OSError, ValueError):
            failed = True
    if failed:
        raise RuntimeError('controller cleanup failed')


class Controller:
    def __init__(self, proc, relay_argv, observer, events):
        self.proc, self.relay_argv = proc, relay_argv
        self.observer, self.events = observer, events
        self.relays = {}
        self.port = None
        self.pending_restart = False
        self.cleaned = None

    def endpoint(self, name, port):
        if not 0 < port < 65536:
            raise ValueError('invalid controller port')
        argv = [*self.relay_argv, str(port)]
        if name not in self.relays:
            self.relays[name] = Relay(argv)
        else:
            self.relays[name].replace(argv)
        return self.relays[name].address.rsplit(':', 1)[1]

    def command(self, line):
        if self.port is None:
            # Startup failures must not send commands to an unready host.
            raise ValueError('controller is not ready')
        command = line.strip()
        if command in ('stop', 'restart'):
            self.relays['host'].replace(None)
            self.pending_restart = command == 'restart'
        self.proc.stdin.write(line)
        self.proc.stdin.flush()

    def output(self, line):
        if line.strip() in ('cleaned', 'startup-failed-cleaned'):
            self.cleaned = line
            return
        if self.port is None:
            self.port = int(line.strip())
            line = self.endpoint('host', self.port) + '\n'
        elif self.pending_restart and line.strip() == 'restart':
            self.endpoint('host', self.port)
            self.pending_restart = False
        sys.stdout.write(line)
        sys.stdout.flush()

    def diagnostic(self, line):
        if self.observer and line.startswith('observer port: '):
            port = self.endpoint('observer', int(line.split(': ', 1)[1]))
            line = 'observer port: ' + port + '\n'
        sys.stderr.write(line)
        sys.stderr.flush()

    def run(self):
        readers = []
        ended = set()
        pending = []
        cancelled = False
        try:
            for name, stream in (('stdout', self.proc.stdout), ('stderr', self.proc.stderr)):
                reader = threading.Thread(target=lines, args=(stream, name, self.events))
                readers.append(reader)
                reader.start()
            threading.Thread(target=lines, args=(sys.stdin, 'stdin', self.events), daemon=True).start()
            while len(ended) < 2:
                name, line = self.events.get()
                if name == 'error':
                    raise RuntimeError('controller stream failed')
                if name in ('stdin', 'cancel'):
                    if line is None:
                        self.proc.stdin.close()
                        cancelled = name == 'cancel'
                        self.proc.wait(timeout=4)
                    elif self.port is None:
                        if sum(map(len, pending)) + len(line) > 65536:
                            raise ValueError('controller input too long')
                        pending.append(line)
                    else:
                        self.command(line)
                elif line is None:
                    ended.add(name)
                elif name == 'stdout':
                    self.output(line)
                    for command in pending:
                        self.command(command)
                    pending.clear()
                else:
                    self.diagnostic(line)
            status = self.proc.wait()
        finally:
            collect(self.proc, self.relays, readers)
        if self.port is None and self.cleaned != 'startup-failed-cleaned\n':
            status = status or 1
        if self.cleaned and status == 0 and not cancelled:
            sys.stdout.write(self.cleaned)
            sys.stdout.flush()
        return status or int(cancelled)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--distro', required=True)
    parser.add_argument('--controller', required=True)
    parser.add_argument('--observer', action='store_true')
    parser.add_argument('args', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    events = queue.Queue()
    previous = {}
    try:
        distro = resolve_wsl_distro({'ECHO_WSL_DISTRO': args.distro})
        base = ['wsl', '-d', distro, '--exec']
        relay_path = subprocess.check_output(
            [*base, 'wslpath', '-u', str(Path(__file__).with_name('wsl_relay.py'))],
            text=True, stderr=subprocess.DEVNULL).strip()
        if not relay_path or '\n' in relay_path:
            raise ValueError('invalid translation')
        for name in ('SIGHUP', 'SIGINT', 'SIGTERM'):
            if hasattr(signal, name):
                number = getattr(signal, name)
                previous[number] = signal.signal(number, lambda *_: events.put(('cancel', None)))
        tail = args.args[1:] if args.args[:1] == ['--'] else args.args
        proc = subprocess.Popen([*base, 'python3', '-u', args.controller, *tail],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, encoding='utf-8', bufsize=1)
        return Controller(proc, [*base, 'python3', '-u', relay_path, '--guest'],
                          args.observer, events).run()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        sys.stderr.write('WSL controller operation failed\n')
        return 1
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)
