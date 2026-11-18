"""T3 provider boundary: real SDP supplied by the local Rust media remote."""
import base64
import hashlib
import http.server
import json
import queue
import select
import socket
import struct
import sys
import threading

answers = queue.Queue()
transcript_input, transcript_output = socket.socketpair()
transcript_count = 0
observed = {'calls': [], 'sidebands': 0, 'external': 0, 'refreshes': 0}
output_lock = threading.Lock()
fault = sys.argv[1] if len(sys.argv) == 2 else None
assert fault in (None, 'call_refused', 'malformed_sdp', 'pending_call')


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
        raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        if self.path == '/token':
            observed['refreshes'] += 1
            self.respond(400, b'{"error":"invalid_grant"}')
            return
        body = json.loads(raw)
        if not self.path.startswith('/backend-api/codex/realtime/calls?'):
            observed['external'] += 1
            self.respond(403, b'{}')
            return
        observed['calls'].append({
            'path': self.path, 'model': body['session']['model'],
            'alpha': self.headers.get('OpenAI-Alpha'),
            'nominated_key': self.headers.get('Authorization') == 'Bearer dummy-local-only',
            'account_header': self.headers.get('ChatGPT-Account-Id'),
        })
        if fault == 'pending_call':
            # The call stays pending until cancellation closes the real HTTP edge.
            self.connection.recv(1)
            self.close_connection = True
            return
        if fault == 'call_refused':
            self.respond(400, b'{"error":"synthetic private provider body"}')
            return
        if fault == 'malformed_sdp':
            self.respond(201, b'synthetic private SDP', 'application/sdp', '/v1/live/rtc_local')
            return
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
        frame(self.connection, {'type': 'session.started', 'session': {'id': 'local-media'}})
        global transcript_count
        try:
            while True:
                readable, _, _ = select.select([self.connection, transcript_input], [], [], 20)
                if not readable:
                    raise AssertionError('sideband control deadline')
                if self.connection in readable and not self.connection.recv(4096):
                    break
                if transcript_input in readable:
                    assert transcript_input.recv(1) == b't'
                    for role, text in [('user', 'ping'), ('assistant', 'echo-ok')]:
                        kind = 'input' if role == 'user' else 'output'
                        frame(self.connection, {'type': kind + '_transcript.added', 'item': {'text': text}})
                        frame(self.connection, {'type': 'turn.done', 'turn': {'role': role, 'transcript': text}})
                    transcript_count += 1
        except OSError:
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
        elif command == {'transcripts': True}:
            transcript_output.sendall(b't')
        elif command == {'transcript_count': True}:
            emit({'transcripts': transcript_count})
        elif command == {'summary': True}:
            expected = {
                'path': '/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas',
                'model': 'gpt-live-1-codex', 'alpha': 'quicksilver=v2',
                'nominated_key': True, 'account_header': None,
            }
            emit({'event': 'provider.snapshot', **observed,
                  'calls': len(observed['calls']),
                  'matching_calls': sum(call == expected for call in observed['calls'])})
        elif command == {'snapshot': True}:
            emit({'event': 'provider.snapshot', **observed})
        else:
            raise AssertionError('unknown fixture command')
finally:
    server.shutdown()
    server.server_close()
    transcript_input.close()
    transcript_output.close()
