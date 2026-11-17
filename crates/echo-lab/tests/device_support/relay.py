"""Windows-only T3 transport: native loopback to guest loopback over WSL stdio."""
import contextlib
import json
import os
import queue
import select
import socket
import struct
import subprocess
import sys
import threading

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
    def __init__(self, peer, argv):
        self.peer = peer
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, bufsize=0)
        self.lock = threading.Lock()
        self.reader = threading.Thread(target=self.receive)
        self.writer = threading.Thread(target=self.send)
        self.reader.start()
        self.writer.start()

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
                data = self.peer.recv(65536)
                self.frame(data)
                if not data:
                    return
        except (OSError, ValueError):
            pass  # Teardown or peer disconnect; receive propagates guest EOF.

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
            with contextlib.suppress(OSError):
                self.peer.shutdown(socket.SHUT_WR)

    def close(self):
        with contextlib.suppress(OSError):
            self.peer.shutdown(socket.SHUT_RDWR)
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
            self.reader.join(timeout=1)
            self.peer.close()
            self.proc.stdout.close()
        if self.writer.is_alive() or self.reader.is_alive():
            raise RuntimeError('relay stream did not collect')


class Relay:
    def __init__(self, argv):
        self.argv = argv
        self.connections = []
        self.listener = socket.socket()
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen()
        self.address = '127.0.0.1:' + str(self.listener.getsockname()[1])
        self.stop_read, self.stop_write = socket.socketpair()
        self.failure = None
        self.thread = threading.Thread(target=self.accept)
        self.thread.start()

    def accept(self):
        try:
            while True:
                ready, _, _ = select.select([self.listener, self.stop_read], [], [])
                if self.stop_read in ready:
                    return
                peer, _ = self.listener.accept()
                try:
                    self.connections.append(Connection(peer, self.argv))
                except BaseException:
                    peer.close()
                    raise
        except (OSError, ValueError) as error:
            self.failure = error

    def close(self):
        self.stop_write.sendall(b'x')
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


def observations(proc, relay_argv, relays, emit, events):
    """Publish a listening native endpoint, retaining every relay until cleanup."""
    try:
        for line in proc.stdout:
            value = json.loads(line)
            if value.get('event') == 'host.ready':
                port = int(value['address'].rsplit(':', 1)[1])
                argv = [*relay_argv, str(port)]
                relay = next((owned for owned in relays if owned.argv == argv), None)
                if relay is None:
                    relay = Relay(argv)
                    relays.append(relay)
                value['address'] = relay.address
            emit(value)
    except (OSError, ValueError, RuntimeError):
        events.put(('error', None))


def forward(argv, relay_argv):
    """Guest controller keeps stdin ownership; only host.ready's address changes."""
    relays = []
    proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            text=True, encoding='utf-8', bufsize=1)
    def commands():
        try:
            while True:
                data = os.read(sys.stdin.fileno(), 65536)
                if not data:
                    break
                while data:
                    count = os.write(proc.stdin.fileno(), data)
                    if not count:
                        raise OSError('controller input closed')
                    data = data[count:]
        except (OSError, ValueError):
            pass
        finally:
            with contextlib.suppress(OSError, ValueError):
                proc.stdin.close()
    threading.Thread(target=commands, daemon=True).start()
    try:
        for line in proc.stdout:
            value = json.loads(line)
            if value.get('event') == 'host.ready':
                port = int(value['address'].rsplit(':', 1)[1])
                relay = Relay([*relay_argv, str(port)])
                relays.append(relay)
                value['address'] = relay.address
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
            for relay in relays:
                relay.close()
