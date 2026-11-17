"""T3 controlled process boundary: no sockets, credentials or provider calls."""
import json
import os
import pathlib
import socket
import sys


def emit(value):
    print(json.dumps(value), flush=True)


mode = pathlib.Path('mode').read_text()
initialized = False
listener = None
for line in sys.stdin:
    request = json.loads(line)
    method = request['method']
    if method == 'initialize':
        assert request['params']['capabilities']['experimentalApi'] is True
        if mode == 'refuse':
            emit({'id': request['id'], 'error': {'code': -32600, 'message': 'private payload'}})
        elif mode == 'eof':
            break
        elif mode.startswith('hold-initialize:'):
            with socket.create_connection(('127.0.0.1', int(mode.split(':')[1]))) as barrier:
                barrier.sendall(b'INITIALIZE_RECEIVED\n')
        else:
            emit({'id': request['id'], 'result': {'userAgent': 'controlled-local'}})
    elif method == 'initialized':
        initialized = True
    else:
        assert initialized, 'request sent before initialized'
        if method == 'crash':
            sys.exit(7)
        if method == 'malformed':
            print('{broken', flush=True)
            continue
        if method == 'oversize':
            print('x' * 4097, flush=True)
            continue
        if method == 'stderr':
            sys.stderr.write('private payload' * 100000)
            sys.stderr.flush()
        if method == 'hold':
            emit({'method': 'holding', 'params': {'id': request['id']}})
            continue
        if method == 'listen':
            listener = socket.socket()
            listener.bind(('127.0.0.1', 0))
            listener.listen()
        emit({'id': request['id'], 'result': {
            'pid': os.getpid(), 'home': os.environ['CODEX_HOME'], 'cwd': os.getcwd(),
            'environment': sorted(os.environ), 'argv': sys.argv[1:], 'initialized': initialized,
            'port': listener.getsockname()[1] if listener else None}})
