"""T3 controlled Wyoming boundary for M5 component proof, never a native host."""
import json
import socket
import sys
import threading

listener = socket.socket()
listener.bind(('127.0.0.1', 0))
listener.listen()
current = None
write_lock = threading.Lock()


def emit(value):
    print(json.dumps(value), flush=True)


def send(connection, kind, data, payload=b''):
    header = {'type': kind, 'data': data, 'payload_length': len(payload)}
    with write_lock:
        connection.sendall(json.dumps(header).encode() + b'\n' + payload)


def serve():
    global current
    while True:
        connection, _ = listener.accept()
        current = connection
        try:
            stream = connection.makefile('rb')
            while line := stream.readline():
                header = json.loads(line)
                stream.read(header.get('data_length', 0))
                stream.read(header.get('payload_length', 0))
                if header['type'] == 'describe':
                    send(connection, 'info', {'satellite': {'name': 'synthetic'}})
                elif header['type'] == 'audio-stop':
                    send(connection, 'played', {})
                elif header['type'] == 'hassmic-end':
                    assert header == {'type': 'hassmic-end', 'data': None,
                                      'data_length': 0, 'payload_length': 0}
        finally:
            connection.close()


threading.Thread(target=serve, daemon=True).start()
emit({'event': 'host.ready', 'address': f'127.0.0.1:{listener.getsockname()[1]}'})
for line in sys.stdin:
    command = json.loads(line)
    if command['action'] == 'wake':
        assert current is not None
        with open(sys.argv[1], encoding='utf-8') as fixture:
            wake = json.loads(fixture.readlines()[1])['header']['data']
        send(current, 'run-pipeline', wake)
        audio = {'rate': 16000, 'width': 2, 'channels': 1}
        send(current, 'audio-start', audio)
        send(current, 'audio-chunk', audio, bytes(640))
    else:
        assert command == {'action': 'registered'}
    emit({'event': 'host.ack'})
listener.close()
