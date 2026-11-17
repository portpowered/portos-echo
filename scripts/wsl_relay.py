"""Windows-only T3 transport: native loopback to guest loopback over WSL stdio."""
import contextlib
from collections import deque
import json
import queue
import select
import socket
import struct
import subprocess
import sys
import threading
import time

STOP = 0xffffffff


def guest(port):
    """Frame stdin so half-close and owner teardown are distinct operations."""
    with socket.create_connection(('127.0.0.1', port)) as peer:
        def read_exact(size):
            data = bytearray()
            while len(data) < size:
                chunk = sys.stdin.buffer.read(size - len(data))
                if not chunk:
                    return None
                data.extend(chunk)
            return data

        events = queue.Queue()
        wake, notify = socket.socketpair()

        def input_frames():
            try:
                while header := read_exact(4):
                    size, = struct.unpack('!I', header)
                    data = None if size == STOP else read_exact(size)
                    events.put(data)
                    notify.sendall(b'x')
                    if data is None:
                        return
            finally:
                events.put(None)
                with contextlib.suppress(OSError):
                    notify.sendall(b'x')

        sender = threading.Thread(target=input_frames)
        sender.start()
        peer.setblocking(False)
        pending = bytearray()
        half_closed = False
        reading = True
        try:
            while True:
                ready, writable, _ = select.select(
                    [wake, *([peer] if reading else [])], [peer] if pending else [], [])
                if wake in ready:
                    wake.recv(65536)
                    while not events.empty():
                        data = events.get_nowait()
                        if data is None:
                            return
                        if data:
                            pending.extend(data)
                        else:
                            half_closed = True
                if peer in writable:
                    count = peer.send(pending)
                    del pending[:count]
                if half_closed and not pending:
                    peer.shutdown(socket.SHUT_WR)
                    half_closed = False
                if peer in ready:
                    data = peer.recv(65536)
                    sys.stdout.buffer.write(struct.pack('!I', len(data)) + data)
                    sys.stdout.buffer.flush()
                    reading = bool(data)
        finally:
            if reading:
                with contextlib.suppress(OSError):
                    sys.stdout.buffer.write(struct.pack('!I', 0))
                    sys.stdout.buffer.flush()
            with contextlib.suppress(OSError):
                peer.shutdown(socket.SHUT_RDWR)
            sender.join()
            wake.close()
            notify.close()


class Connection:
    def __init__(self, peer, argv, record=lambda **_: None):
        self.record = record
        self.peer = peer
        self.stop_read, self.stop_write = socket.socketpair()
        try:
            self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                         stderr=subprocess.DEVNULL, bufsize=0)
        except BaseException:
            self.stop_read.close()
            self.stop_write.close()
            raise
        self.record(phase="spawn", role="relay-guest", pid=self.proc.pid)
        self.lock = threading.Lock()
        self.reader = threading.Thread(target=self.receive)
        self.writer = threading.Thread(target=self.send)
        try:
            self.reader.start()
            self.writer.start()
        except BaseException:
            self.close()
            raise

    def frame(self, data=None):
        with self.lock:
            value = struct.pack('!I', STOP if data is None else len(data)) + (data or b'')
            while value:
                count = self.proc.stdin.write(value)
                if not count:
                    raise OSError('relay input closed')
                value = value[count:]

    def send(self):
        try:
            while True:
                ready, _, _ = select.select([self.peer, self.stop_read], [], [])
                if self.stop_read in ready:
                    return
                data = self.peer.recv(65536)
                self.frame(data)
                if not data:
                    return
        except (OSError, ValueError):
            with contextlib.suppress(OSError, ValueError):
                self.frame(None)  # A native reset must release the guest host socket.

    def receive(self):
        def read_exact(size):
            data = bytearray()
            while len(data) < size:
                chunk = self.proc.stdout.read(size - len(data))
                if not chunk:
                    raise OSError('relay output closed')
                data.extend(chunk)
            return data
        try:
            while True:
                size, = struct.unpack('!I', read_exact(4))
                if not size:
                    break
                self.peer.sendall(read_exact(size))
        except (OSError, ValueError):
            # Keep draining after a native peer disappears so guest stdout can
            # never prevent its stdin-EOF teardown.
            with contextlib.suppress(OSError, ValueError):
                while self.proc.stdout.read(65536):
                    pass
        finally:
            self.record(phase="eof", role="relay-guest", pid=self.proc.pid, status=self.proc.poll())
            with contextlib.suppress(OSError):
                self.peer.shutdown(socket.SHUT_WR)

    def close(self):
        self.stop_write.sendall(b'x')
        with contextlib.suppress(OSError):
            self.peer.shutdown(socket.SHUT_RDWR)
        if self.writer.ident is not None:
            self.writer.join(timeout=1)
        with contextlib.suppress(OSError, ValueError):
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            raise RuntimeError('relay guest did not collect') from None
        finally:
            self.record(phase="collected", role="relay-guest", pid=self.proc.pid, status=self.proc.poll())
            if self.reader.ident is not None:
                self.reader.join(timeout=1)
            self.peer.close()
            self.proc.stdout.close()
            self.stop_read.close()
            self.stop_write.close()
        if self.writer.is_alive() or self.reader.is_alive():
            raise RuntimeError('relay stream did not collect')


class Relay:
    def __init__(self, argv):
        self.argv = argv
        self.connections = []
        self.guard = threading.Lock()
        self.mutation = threading.Lock()
        self.closing = False
        self.diagnostics = deque(maxlen=64)
        self.generation = 1
        self.sequence = 0
        self.awaiting_ready = 0
        self.listener = socket.socket()
        try:
            self.listener.bind(('127.0.0.1', 0))
            self.listener.listen()
        except BaseException:
            self.listener.close()
            raise
        self.address = '127.0.0.1:' + str(self.listener.getsockname()[1])
        self.failure = None
        self.thread = threading.Thread(target=self.accept)
        try:
            self.stop_read, self.stop_write = socket.socketpair()
            self.stop_write.setblocking(False)
            self.thread.start()
        except BaseException:
            self.listener.close()
            if hasattr(self, 'stop_read'):
                self.stop_read.close()
                self.stop_write.close()
            raise

    def record(self, **event):
        self.diagnostics.append(dict(time=time.monotonic(), generation=self.generation, **event))

    def wake(self):
        with contextlib.suppress(BlockingIOError):
            self.stop_write.send(b'x')

    def accept(self):
        try:
            while True:
                with self.guard:
                    if self.closing:
                        return
                    inputs = [self.stop_read, *([self.listener] if self.argv is not None else [])]
                ready, _, _ = select.select(inputs, [], [])
                if self.stop_read in ready:
                    self.stop_read.recv(65536)
                with self.guard:
                    if self.closing:
                        return
                    # Recheck after select: replacement can invalidate a ready
                    # listener. Leave unassigned peers in the bounded backlog.
                    if self.argv is None or self.listener not in ready:
                        continue
                    peer, _ = self.listener.accept()
                    self.sequence += 1
                    connection_id = self.sequence
                    generation = self.generation
                    def record(connection=connection_id, generation=generation, **event):
                        self.record(connection=connection, assigned_generation=generation, **event)
                    record(phase="accept", role="native-peer", target=self.argv[-1])
                    try:
                        self.connections.append(Connection(peer, self.argv, record))
                    except BaseException:
                        peer.close()
                        raise
        except (OSError, ValueError, RuntimeError) as error:
            self.failure = error
            self.record(phase="failed", role="relay")

    def replace(self, argv, *, awaiting_ready=False):
        """Suspend acceptance before collecting; only readiness resumes it."""
        with self.mutation:
            with self.guard:
                if self.closing:
                    raise RuntimeError('relay is closed')
                if awaiting_ready:
                    self.awaiting_ready += 1
                elif argv is not None and self.awaiting_ready:
                    self.awaiting_ready -= 1
                    if self.awaiting_ready:
                        self.record(phase="superseded-ready", role="relay")
                        return
                self.argv = None
                self.record(phase="hold", role="relay")
                connections, self.connections = self.connections, []
                self.wake()
            failures = []
            for connection in connections:
                try:
                    connection.close()
                except (OSError, ValueError, RuntimeError) as error:
                    failures.append(error)
            if failures:
                raise RuntimeError('relay cleanup failed')
            with self.guard:
                self.generation += 1
                self.argv = argv
                self.record(phase="ready" if argv is not None else "suspended", role="relay")
                self.wake()

    def close(self):
        with self.mutation:
            with self.guard:
                if self.closing:
                    return
                self.closing = True
                self.wake()
            self.thread.join()
            self.listener.close()
            self.stop_write.close()
            self.stop_read.close()
            failures = []
            for connection in self.connections:
                try:
                    connection.close()
                except (OSError, ValueError, RuntimeError) as error:
                    failures.append(error)
            if self.failure or failures:
                raise RuntimeError('relay cleanup failed')


def rewrite_ready(value, relay_argv, relays):
    if value.get('event') == 'host.ready':
        port = int(value['address'].rsplit(':', 1)[1])
        argv = [*relay_argv, str(port)]
        if not relays:
            relay = Relay(argv)
            relays.append(relay)
        else:
            relay = relays[0]
            relay.replace(argv)
        value['address'] = relay.address


def close_relays(relays):
    failures = []
    for relay in relays:
        try:
            relay.close()
        except (OSError, ValueError, RuntimeError) as error:
            failures.append(error)
    if failures:
        raise RuntimeError('relay cleanup failed') from None


def observations(proc, relay_argv, relays, emit, events):
    """Publish a listening native endpoint, retaining every relay until cleanup."""
    try:
        for line in proc.stdout:
            value = json.loads(line)
            rewrite_ready(value, relay_argv, relays)
            emit(value)
    except (OSError, ValueError, RuntimeError):
        events.put(('error', None))


def forward_command(line, stream, relays):
    """Invalidate before the guest can act; preserve all forwarded bytes."""
    if len(line) > 65536:
        raise ValueError('controller line too long')
    if json.loads(line).get('action') in ('kill', 'restart'):
        for relay in relays:
            relay.replace(None, awaiting_ready=True)
    while line:
        count = stream.write(line)
        if not count:
            raise OSError('controller input closed')
        line = line[count:]
    stream.flush()


def diagnostic_tail(relays):
    for relay in relays:
        tail = getattr(relay, "diagnostics", ())
        for event in list(tail) if isinstance(tail, deque) else ():
            sys.stderr.write('relay: ' + json.dumps(event) + '\n')
    sys.stderr.flush()


def forward(argv, relay_argv):
    """Guest controller keeps stdin ownership; only host.ready's address changes."""
    relays = []
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            text=True, encoding='utf-8', bufsize=1)
    def commands():
        try:
            while line := sys.stdin.buffer.readline(65537):
                # Keep writes unbuffered so owner EOF/cleanup cannot wait on a
                # TextIOWrapper lock held by this forwarding thread.
                forward_command(line, proc.stdin.buffer.raw, relays)
        except (OSError, ValueError, RuntimeError):
            pass
        finally:
            with contextlib.suppress(OSError, ValueError):
                proc.stdin.close()
    threading.Thread(target=commands, daemon=True).start()
    try:
        for line in proc.stdout:
            value = json.loads(line)
            rewrite_ready(value, relay_argv, relays)
            print(json.dumps(value), flush=True)
        return proc.wait()
    finally:
        # Closing the control pipe requests pinned-host collection even if the
        # caller is still open and parsing/forwarding fails.
        with contextlib.suppress(OSError, ValueError):
            proc.stdin.close()
        try:
            proc.wait(timeout=4)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
            raise RuntimeError('relay controller did not collect') from None
        finally:
            proc.stdout.close()
            try:
                close_relays(relays)
            finally:
                diagnostic_tail(relays)
                if isinstance(proc.pid, int):
                    sys.stderr.write('relay: ' + json.dumps(dict(role="host-controller", pid=proc.pid,
                                                               phase="collected", status=proc.poll())) + '\n')


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--guest':
        guest(int(sys.argv[2]))
    else:
        from wsl_controller import main
        sys.exit(main())
