"""T3 provider boundary: real SDP supplied by the local Rust media remote."""
import base64
import hashlib
import http.server
import json
import queue
import socket
import struct
import sys
import threading

answers = queue.Queue()
transcripts = threading.Event()
observed = {'calls': [], 'sidebands': 0, 'external': 0}
output_lock = threading.Lock()


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
        if not self.path.startswith('/backend-api/codex/realtime/calls?'):
            observed['external'] += 1
            self.respond(403, b'{}')
            return
        observed['calls'].append({
            'path': self.path, 'model': body['session']['model'],
            'alpha': self.headers.get('OpenAI-Alpha'),
        })
        emit({'offer': body['sdp']})
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
        frame(self.connection, {'type': 'session.started', 'session': {'id': 'local-media'}})
        assert transcripts.wait(20), 'transcript command missing'
        for role, text in [('user', 'ping'), ('assistant', 'echo-ok')]:
            kind = 'input' if role == 'user' else 'output'
            frame(self.connection, {'type': kind + '_transcript.added', 'item': {'text': text}})
            frame(self.connection, {'type': 'turn.done', 'turn': {'role': role, 'transcript': text}})
        self.connection.settimeout(20)
        try:
            while self.connection.recv(4096):
                pass
        except (OSError, socket.timeout):
            pass
        self.close_connection = True


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
emit({'origin': f'http://127.0.0.1:{server.server_port}'})
try:
    for line in sys.stdin:
        command = json.loads(line)
        if 'answer' in command:
            answers.put(command['answer'])
        elif command == {'transcripts': True}:
            transcripts.set()
        elif command == {'snapshot': True}:
            emit(observed)
        else:
            raise AssertionError('unknown fixture command')
finally:
    server.shutdown()
    server.server_close()
