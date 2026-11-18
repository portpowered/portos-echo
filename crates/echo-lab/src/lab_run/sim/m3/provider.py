"""T3 sideband boundary for the pinned Codex child; no account or remote API."""
import base64
import hashlib
import http.server
import json
from pathlib import Path
import queue
import select
import socket
import struct
import sys
import threading

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'm4' / 'codex'))
from tools_provider import read_frame

answers = queue.Queue()
events = queue.Queue()
wake_read, wake_write = socket.socketpair()
output_lock = threading.Lock()
observed = {'calls': 0, 'sidebands': 0, 'external': 0, 'appends': [], 'models': 0, 'released': 0}
release_model = threading.Event()


def emit(value):
    with output_lock:
        print(json.dumps(value), flush=True)


def frame(connection, value):
    data = json.dumps(value).encode()
    header = bytes([129, len(data)]) if len(data) < 126 else bytes([129, 126]) + struct.pack('!H', len(data))
    connection.sendall(header + data)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def respond(self, code, data, kind='application/json', location=None):
        self.send_response(code)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(data)))
        if location:
            self.send_header('Location', location)
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()

    def do_CONNECT(self):
        observed['external'] += 1
        self.respond(403, b'{}')

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        if self.path == '/v1/responses':
            if body.get('model') != 'gpt-5.4':
                emit({'event': 'provider.failure', 'reason': 'wrong_model'})
                self.respond(400, b'{"error":{"message":"wrong fixture model"}}')
                return
            observed['models'] += 1
            assert observed['models'] == 1, 'unexpected model retry'
            emit({'event': 'provider.model_started'})
            assert release_model.wait(20), 'late model release deadline'
            item = {'type': 'message', 'id': 'stale-message', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': 'stale delegated answer', 'annotations': []}]}
            response = {'id': 'stale-response', 'status': 'completed', 'output': [item]}
            values = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}},
                      {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                      {'type': 'response.completed', 'response': response}]
            data = ''.join('event: ' + value['type'] + '\ndata: ' + json.dumps(value) + '\n\n' for value in values).encode()
            try:
                self.respond(200, data, 'text/event-stream')
            except (BrokenPipeError, ConnectionResetError):
                pass  # A cancelled pinned request may already have closed HTTP.
            observed['released'] += 1
            emit({'event': 'provider.model_released'})
            return
        if not self.path.startswith('/backend-api/codex/realtime/calls?'):
            observed['external'] += 1
            self.respond(403, b'{}')
            return
        observed['calls'] += 1
        emit({'event': 'provider.offer', 'offer': body['sdp']})
        self.respond(201, answers.get(timeout=20).encode(), 'application/sdp', '/v1/live/rtc_local')

    def do_GET(self):
        if self.path != '/v1/live/rtc_local':
            observed['external'] += 1
            self.respond(403, b'{}')
            return
        observed['sidebands'] += 1
        key = self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
        self.end_headers()
        frame(self.connection, {'type': 'session.started', 'session': {'id': 'local-barge'}})
        emit({'event': 'provider.sideband'})
        try:
            while True:
                readable, _, _ = select.select([self.connection, wake_read], [], [], 20)
                if not readable:
                    raise AssertionError('sideband control deadline')
                if self.connection in readable:
                    opcode, data = read_frame(self.connection)
                    if opcode == 8:
                        break
                    if opcode == 1:
                        value = json.loads(data)
                        if value.get('type') == 'delegation.context.append':
                            observed['appends'].append(value)
                if wake_read in readable:
                    assert wake_read.recv(1) == b'e'
                    command = events.get_nowait()
                    for value in command['events']:
                        frame(self.connection, value)
                    emit({'event': 'provider.sent', 'tag': command['tag']})
        except (OSError, EOFError):
            pass
        self.close_connection = True


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
emit({'event': 'provider.ready', 'origin': f'http://127.0.0.1:{server.server_port}'})
try:
    for line in sys.stdin:
        command = json.loads(line)
        if 'answer' in command:
            answers.put(command['answer'])
        elif 'events' in command:
            events.put(command)
            wake_write.sendall(b'e')
        elif command == {'snapshot': True}:
            emit({'event': 'provider.snapshot', **observed})
        elif command == {'release_model': True}:
            release_model.set()
        else:
            raise AssertionError('unknown fixture command')
finally:
    release_model.set()
    server.shutdown()
    server.server_close()
    wake_read.close()
    wake_write.close()
