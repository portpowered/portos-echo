"""Controlled local-process substitutes for the T3 WSL transport boundary."""
SERVER = r'''
import socket, sys, threading
listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(('127.0.0.1', int(sys.argv[1])))
listener.listen()
print(listener.getsockname()[1], flush=True)
def serve(peer):
    with peer:
        try:
            data = bytearray()
            while part := peer.recv(65536):
                data.extend(part)
            peer.sendall(bytes(data)[::-1])
        except ConnectionError:
            pass
while True:
    threading.Thread(target=serve, args=(listener.accept()[0],), daemon=True).start()
'''

CONTROLLER = r'''
import select, socket, subprocess, sys, threading
from wsl_relay_test_support import SERVER
host = None
port = 0
def start():
    global host, port
    host = subprocess.Popen([sys.executable, '-u', '-c', SERVER, str(port)],
                            stdout=subprocess.PIPE, text=True)
    port = int(host.stdout.readline())
    print('host pid: ' + str(host.pid), file=sys.stderr, flush=True)
def stop():
    if host.poll() is None:
        host.kill()
    host.wait()
    host.stdout.close()
observer = socket.socket()
observer.bind(('127.0.0.1', 0))
observer.listen()
cancel, notify = socket.socketpair()
def snapshots():
    while True:
        ready, _, _ = select.select([observer, cancel], [], [])
        if cancel in ready:
            return
        with observer.accept()[0] as peer:
            peer.sendall(b'{"state":"controlled"}')
worker = threading.Thread(target=snapshots)
worker.start()
print('observer port: ' + str(observer.getsockname()[1]), file=sys.stderr, flush=True)
print('controller pid: ' + str(__import__('os').getpid()), file=sys.stderr, flush=True)
try:
    start()
    print(port, flush=True)
    for line in sys.stdin:
        command = line.strip()
        if command in ('stop', 'kill'):
            stop()
        elif command == 'restart':
            start()
        print(command, flush=True)
finally:
    stop()
    notify.sendall(b'x')
    worker.join()
    observer.close()
    cancel.close()
    notify.close()
print('cleaned', flush=True)
'''

BOOTSTRAP = r'''
import os, subprocess, sys, threading
from unittest import mock
from wsl_controller import main
original = subprocess.Popen
owned = []
def spawn(argv, *args, **kwargs):
    if argv[0] == 'wsl':
        assert argv[1:4] == ['-d', 'controlled', '--exec']
        argv = [sys.executable, *argv[5:]]
    proc = original(argv, *args, **kwargs)
    owned.append(proc)
    return proc
with mock.patch.object(subprocess, 'Popen', side_effect=spawn), \
     mock.patch.object(subprocess, 'check_output', return_value=sys.argv[1] + '\n'):
    sys.argv = ['relay', '--distro', 'controlled', '--controller', sys.argv[2], '--observer', '--']
    status = main()
assert all(proc.poll() is not None for proc in owned), 'uncollected child'
assert not [t for t in threading.enumerate() if t is not threading.main_thread() and not t.daemon]
print('collected: ' + ','.join(str(proc.pid) for proc in owned), file=sys.stderr, flush=True)
sys.exit(status)
'''
