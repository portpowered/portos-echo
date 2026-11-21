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
call_voices = []
output_lock = threading.Lock()
delegation = None
manual_cases = {}
session = None


def emit(value):
    with output_lock:
        print(json.dumps(value), flush=True)


def frame(connection, value):
    data = json.dumps(value).encode()
    header = bytes([129, len(data)]) if len(data) < 126 else bytes([129, 126]) + struct.pack('!H', len(data))
    connection.sendall(header + data)


def observe_append(connection, value):
    identity = value['delegation_item_id']
    case = manual_cases.get(identity) if manual_cases else delegation
    try:
        assert case is not None, 'append for unknown delegation'
        with case.lock:
            case.append(value)
    except AssertionError as error:
        emit({'failure': str(error), 'delegation': identity})
        return
    if manual_cases:
        emit({'append': identity})
    else:
        connection.transcripts(case.reply)


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

    def model_response(self, body):
        identity = delegation.identity
        try:
            case = delegation
            if manual_cases:
                identity = tools_provider.model_identity(body)
                case = manual_cases.get(identity)
                assert case is not None, 'model request for unknown delegation'
            data = case.accept_model(body)
            if case.release is not None:
                assert case.release.wait(timeout=20), 'interrupted model release deadline'
            elif case.exhausted:
                emit({'model_started': True})
        except AssertionError as error:
            emit({'failure': str(error), 'delegation': identity})
            self.respond(400, b'{"error":{"message":"fixture contract mismatch"}}')
            return
        try:
            self.respond(200, data, 'text/event-stream')
        except OSError:
            # The interrupted pin can close its HTTP stream before release.
            if case.release is None:
                raise

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        if self.path == '/skills-mcp' and isinstance(delegation, tools_provider.RunnerSkills):
            code, reply = delegation.mcp_response(body)
            self.respond(code, b'' if reply is None else json.dumps(reply).encode())
            return
        if self.path == '/v1/responses' and delegation is not None:
            self.model_response(body)
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
        global session
        session = {key: body['session'].get(key) for key in ('instructions', 'initial_items')}
        call_voices.append(body['session'].get('audio', {}).get('output', {}).get('voice'))
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
                            observe_append(self, value)
                if transcript_input in readable:
                    assert transcript_input.recv(1) == b't'
                    if delegation is None:
                        self.transcripts('echo-ok')
                    else:
                        frame(self.connection, delegation.begin())
                    transcript_count += 1
        except (OSError, EOFError):
            pass
        finally:
            if manual_cases:
                emit({'sideband_closed': True})
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
            identity = command.get('identity')
            assert delegation is None or (identity and identity not in manual_cases)
            history = [] if delegation is None else delegation.history + delegation.outputs
            if 'native_extras' in command:
                delegation = tools_provider.NativeExtras(command['tools'], command['native_extras'])
                continue
            delegation = tools_provider.Delegation(
                command['tools'], command['negative'], command.get('exhausted', False),
                command.get('model', 'gpt-5.4'), command.get('web_search', 'live'), identity, history)
            if identity:
                manual_cases[identity] = delegation
                emit({'initialized': identity})
        elif 'runner_skills' in command:
            assert delegation is None
            delegation = tools_provider.RunnerSkills(command['runner_skills'], command.get('skills_mcp'))
            manual_cases[delegation.identity] = delegation
            emit({'initialized': delegation.identity})
        elif 'end_call' in command:
            assert delegation is None
            delegation = tools_provider.EndCall(command['end_call'])
            manual_cases[delegation.identity] = delegation
            emit({'initialized': delegation.identity})
        elif command == {'transcripts': True}:
            transcript_output.sendall(b't')
        elif command == {'transcript_count': True}:
            emit({'transcripts': transcript_count})
        elif command == {'append_receipts': True}:
            emit({'append_receipts': delegation.append_receipts})
        elif command == {'session': True}:
            emit({'session': session})
        elif command == {'call_voices': True}:
            emit({'call_voices': call_voices})
        elif 'release_model' in command:
            identity = command['release_model']
            case = manual_cases[identity]
            case.release_model()
            emit({'released': identity, 'ingress': case.ingress})
        elif 'snapshot' in command:
            selection = command['snapshot']
            case = delegation if selection is True else manual_cases[selection]
            if case is None:
                emit(observed)
            else:
                with case.lock:
                    emit({**observed, **case.snapshot()})
        else:
            raise AssertionError('unknown fixture command')
finally:
    server.shutdown()
    server.server_close()
    transcript_input.close()
    transcript_output.close()
