"""T3 provider for persistent session journeys; no account or remote API.

Each delegated turn follows a fixed local script: zero or more session-tool
calls, then one spoken answer. Tool outputs are observed exactly as the pinned
engine returns them, so the driver can check the bridge's own results.
"""
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
from tools_provider import function_tools, read_frame

answers = queue.Queue()
commands = queue.Queue()
heard = threading.Event()
wake_read, wake_write = socket.socketpair()
output_lock = threading.Lock()
observed = {'calls': 0, 'sidebands': 0, 'external': 0, 'models': [], 'delegations': [], 'appends': []}
script = None


def emit(value):
    with output_lock:
        print(json.dumps(value), flush=True)


def frame(connection, value):
    data = json.dumps(value).encode()
    header = bytes([129, len(data)]) if len(data) < 126 else bytes([129, 126]) + struct.pack('!H', len(data))
    connection.sendall(header + data)


class Script:
    """Turn i answers with turns[i]: [{'call': name, 'arguments': {...}}..., {'say': text}].

    A step may carry 'heard': user speech delivered while its model request is pending."""

    def __init__(self, turns):
        self.turns = turns
        self.step = 0

    def turn(self):
        return len(observed['delegations']) - 1

    def respond(self, body):
        assert body['tools'] == function_tools([]), 'session catalog mismatch'
        assert body['model'] == 'gpt-5.4', 'wrong delegated model'
        turn = self.turn()
        assert 0 <= turn < len(self.turns) and len(observed['appends']) == turn, 'model outside a delegated turn'
        steps = self.turns[turn]
        assert self.step < len(steps), 'unexpected model retry'
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output']
        mine = [item for item in outputs if item['call_id'].startswith(f'session-{turn}-')]
        assert len(mine) == self.step, 'tool output count mismatch'
        for position, item in enumerate(mine):
            assert item['call_id'] == f'session-{turn}-{position}', 'tool identity mismatch'
        step = steps[self.step]
        if 'call' in step:
            item = {'type': 'function_call', 'id': f'fc-{turn}-{self.step}', 'call_id': f'session-{turn}-{self.step}',
                    'name': step['call'], 'arguments': json.dumps(step['arguments']), 'status': 'completed'}
        else:
            item = {'type': 'message', 'id': f'msg-{turn}', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': step['say'], 'annotations': []}]}
        self.step += 1
        if 'heard' in step:
            # The turn is in flight: this model request is still unanswered.
            commands.put({'heard': step['heard']})
            wake_write.sendall(b'e')
            assert heard.wait(20), 'sideband did not deliver the overlapping speech'
        observed['models'].append({'turn': turn, 'tools': [tool['name'] for tool in body['tools']]})
        emit({'event': 'provider.model', 'turn': turn, 'outputs': [item['output'] for item in mine]})
        response = {'id': f'response-{turn}-{self.step}', 'status': 'completed', 'output': [item]}
        events = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}},
                  {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                  {'type': 'response.completed', 'response': response}]
        return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events).encode()

    def begin(self, turn):
        assert turn == len(observed['delegations']) == len(observed['appends']), 'overlapping delegation'
        self.step = 0
        name = f'session-turn-{turn}'
        observed['delegations'].append(name)
        return {'type': 'delegation.created', 'item': {
            'type': 'delegation', 'target': 'client', 'id': name,
            'content': [{'type': 'input_text', 'text': f'Answer session turn {turn}.'}]}}

    def append(self, value):
        turn = self.turn()
        assert self.step == len(self.turns[turn]), 'append before the scripted answer'
        assert value['delegation_item_id'] == f'session-turn-{turn}', 'append identity mismatch'
        assert value['content'] == [{'type': 'input_text', 'text': self.turns[turn][-1]['say']}], 'append content mismatch'
        observed['appends'].append(value['delegation_item_id'])

    def transcripts(self, turn):
        values = []
        for role, text in [('user', 'ping'), ('assistant', self.turns[turn][-1]['say'])]:
            kind = 'input' if role == 'user' else 'output'
            values.append({'type': kind + '_transcript.added', 'item': {'text': text}})
            values.append({'type': 'turn.done', 'turn': {'role': role, 'transcript': text}})
        return values


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
        if self.path == '/v1/responses' and script is not None:
            try:
                self.respond(200, script.respond(body), 'text/event-stream')
            except AssertionError as error:
                emit({'event': 'provider.failure', 'failure': str(error)})
                self.respond(400, b'{"error":{"message":"fixture contract mismatch"}}')
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
        frame(self.connection, {'type': 'session.started', 'session': {'id': 'local-session'}})
        try:
            while True:
                readable, _, _ = select.select([self.connection, wake_read], [], [], 30)
                if not readable:
                    raise AssertionError('sideband control deadline')
                if self.connection in readable:
                    opcode, data = read_frame(self.connection)
                    if opcode == 8:
                        break
                    if opcode == 1:
                        value = json.loads(data)
                        if value.get('type') == 'delegation.context.append':
                            try:
                                script.append(value)
                            except AssertionError as error:
                                emit({'event': 'provider.failure', 'failure': str(error)})
                                continue
                            emit({'event': 'provider.append', 'delegation_id': value['delegation_item_id']})
                if wake_read in readable:
                    assert wake_read.recv(1) == b'e'
                    command = commands.get_nowait()
                    if 'delegate' in command:
                        value = script.begin(command['delegate'])
                        frame(self.connection, value)
                        emit({'event': 'provider.delegation', 'delegation_id': value['item']['id']})
                    elif 'heard' in command:
                        frame(self.connection, {'type': 'input_transcript.added', 'item': {'text': command['heard']}})
                        frame(self.connection, {'type': 'turn.done',
                                                'turn': {'role': 'user', 'transcript': command['heard']}})
                        heard.set()
                        emit({'event': 'provider.heard'})
                    else:
                        for value in script.transcripts(command['transcripts']):
                            frame(self.connection, value)
                        emit({'event': 'provider.transcripts', 'turn': command['transcripts']})
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
        elif 'script' in command:
            assert script is None
            script = Script(command['script'])
        elif 'delegate' in command or 'transcripts' in command:
            commands.put(command)
            wake_write.sendall(b'e')
        elif command == {'snapshot': True}:
            emit({'event': 'provider.snapshot', **observed})
        else:
            raise AssertionError('unknown fixture command')
finally:
    server.shutdown()
    server.server_close()
    wake_read.close()
    wake_write.close()
