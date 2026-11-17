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


JSON_CONTROLLER = r'''
import json, subprocess, sys
from wsl_relay_test_support import SERVER
host = None
def start():
    global host
    host = subprocess.Popen([sys.executable, '-u', '-c', SERVER, '0'],
                            stdout=subprocess.PIPE, text=True)
    print(json.dumps({'event': 'host.ready', 'address': '127.0.0.1:' + host.stdout.readline().strip()}), flush=True)
def stop():
    host.kill()
    host.wait()
    host.stdout.close()
start()
try:
    for line in sys.stdin:
        action = json.loads(line)['action']
        if action == 'kill':
            stop()
        elif action == 'release':
            start()
        print(json.dumps({'event': 'host.ack', 'action': action}), flush=True)
finally:
    if host.poll() is None:
        stop()
'''


def exercise_json_forward(test, root):
    """Actual JSON stdio owner, native relay and framed guest, without WSL."""
    import json
    import os
    import queue
    import threading
    import socket
    import subprocess
    import sys
    import tempfile
    with tempfile.TemporaryDirectory() as folder:
        controller = root.__class__(folder) / 'controller.py'
        controller.write_text(JSON_CONTROLLER)
        code = ('import sys; from wsl_relay import forward; '
                'sys.exit(forward([sys.executable, "-u", sys.argv[1]], '
                '[sys.executable, "-u", sys.argv[2], "--guest"]))')
        proc = subprocess.Popen([sys.executable, '-u', '-c', code, str(controller), str(root / 'wsl_relay.py')],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, env=dict(os.environ, PYTHONPATH=str(root)))
        out = queue.Queue()
        reader = threading.Thread(target=test.read, args=(proc.stdout, out))
        reader.start()
        try:
            initial = json.loads(out.get(timeout=5))
            port = int(initial['address'].rsplit(':', 1)[1])
            proc.stdin.write('{"action":"kill"}\n')
            proc.stdin.flush()
            test.assertEqual(json.loads(out.get(timeout=5)), {'event': 'host.ack', 'action': 'kill'})
            with socket.create_connection(('127.0.0.1', port), timeout=5) as client:
                client.sendall(b'json-gap')
                client.shutdown(socket.SHUT_WR)
                proc.stdin.write('{"action":"release"}\n')
                proc.stdin.flush()
                test.assertEqual(json.loads(out.get(timeout=5)), initial)
                test.assertEqual(json.loads(out.get(timeout=5)), {'event': 'host.ack', 'action': 'release'})
                test.assertEqual(client.recv(8), b'pag-nosj')
                test.assertEqual(client.recv(1), b'')
            proc.stdin.close()
            test.assertEqual(proc.wait(timeout=5), 0)
            diagnostics = proc.stderr.read()
            test.assertIn('"role": "host-controller"', diagnostics)
            test.assertIn('"phase": "collected", "status": 0', diagnostics)
        finally:
            test.dispose(proc)
            reader.join(timeout=5)
            test.assertFalse(reader.is_alive())
