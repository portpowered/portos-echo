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
import tools_provider

answers = queue.Queue()
transcript_input, transcript_output = socket.socketpair()
transcript_count = 0
observed = {'calls': [], 'sidebands': 0, 'external': 0}
output_lock = threading.Lock()
delegation = None
manual = False


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
        if self.path == '/v1/responses' and delegation is not None:
            try:
                data = delegation.respond(body)
                if delegation.exhausted:
                    emit({'model_started': True})
                self.respond(200, data, 'text/event-stream')
            except AssertionError as error:
                delegation.failure = str(error)
                emit({'failure': delegation.failure})
                self.respond(400, b'{"error":{"message":"fixture contract mismatch"}}')
            return
        if self.path == '/token':
            assert body['grant_type'] == 'refresh_token'
            assert body['refresh_token'] == 'synthetic-refresh'
            observed['refreshes'] = observed.get('refreshes', 0) + 1
            self.close_connection = True
            self.respond(401, b'{"error":{"code":"refresh_token_expired"}}')
            return
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
        global transcript_count
        try:
            while True:
                readable, _, _ = select.select([self.connection, transcript_input], [], [], 20)
                if not readable:
                    raise AssertionError('sideband control deadline')
                if self.connection in readable:
                    opcode, data = tools_provider.read_frame(self.connection)
                    if opcode == 8:
                        break
                    if opcode == 1 and delegation is not None:
                        value = json.loads(data)
                        if value.get('type') == 'delegation.context.append':
                            delegation.append(value)
                            if manual:
                                emit({'append': value['delegation_item_id']})
                            else:
                                self.transcripts(delegation.reply)
                if transcript_input in readable:
                    assert transcript_input.recv(1) == b't'
                    if delegation is None:
                        self.transcripts('echo-ok')
                    else:
                        frame(self.connection, delegation.begin())
                    transcript_count += 1
        except (OSError, EOFError):
            pass
        self.close_connection = True

    def transcripts(self, reply):
        for role, text in [('user', 'ping'), ('assistant', reply)]:
            kind = 'input' if role == 'user' else 'output'
            frame(self.connection, {'type': kind + '_transcript.added', 'item': {'text': text}})
            frame(self.connection, {'type': 'turn.done', 'turn': {'role': role, 'transcript': text}})


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
emit({'origin': f'http://127.0.0.1:{server.server_port}'})
try:
    for line in sys.stdin:
        command = json.loads(line)
        if 'answer' in command:
            answers.put(command['answer'])
        elif 'tools' in command:
            assert delegation is None or manual
            history = [] if delegation is None else delegation.history + delegation.outputs
            manual = command.get('manual', False)
            delegation = tools_provider.Delegation(command['tools'], command['negative'], command.get('exhausted', False), command.get('model', 'gpt-5.4'))
            if manual:
                delegation.identity = command['identity']
                delegation.call_id = command['identity'] + '-tool'
                delegation.history = history
                emit({'initialized': command['identity']})
        elif command == {'transcripts': True}:
            transcript_output.sendall(b't')
        elif command == {'transcript_count': True}:
            emit({'transcripts': transcript_count})
        elif command == {'snapshot': True}:
            emit(observed if delegation is None else {**observed, **delegation.snapshot()})
        else:
            raise AssertionError('unknown fixture command')
finally:
    server.shutdown()
    server.server_close()
    transcript_input.close()
    transcript_output.close()
