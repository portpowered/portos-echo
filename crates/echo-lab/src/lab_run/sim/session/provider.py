"""T3 provider for persistent session journeys; no account or remote API.

Each delegated turn follows a fixed local script: zero or more session-tool
calls, then one spoken answer. Tool outputs are observed exactly as the pinned
engine returns them, so the driver can check the bridge's own results.
"""
import base64
from datetime import datetime, timezone
import hashlib
import http.server
import json
from pathlib import Path
import queue
import re
import select
import socket
import struct
import sys
import threading
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'm4' / 'codex'))
from tools_provider import function_tools, read_frame

answers = queue.Queue()
commands = queue.Queue()
heard = threading.Event()
release_goodbye = threading.Event()
wake_read, wake_write = socket.socketpair()
output_lock = threading.Lock()
observed = {'calls': 0, 'sidebands': 0, 'external': 0, 'models': [], 'delegations': [], 'appends': [],
            'model_requests': 0, 'time_wire': [], 'media_time_turns': [], 'media_time_transcripts': []}
script = None
TIME_RULE = ('Always answer time, date, and day-of-week questions directly '
             'from the call-start context and never hand them off.')
UTC_CAVEAT = "When no operator time zone is configured, UTC does not establish the user's local time zone."
SEARCH_GUIDANCE = 'When current facts need a lookup, the background agent can search the web'


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
        self.instructions = ''
        self.initial_history = ''

    def turn(self):
        return len(observed['delegations']) - 1

    def respond(self, body):
        turn = self.turn()
        assert 0 <= turn < len(self.turns), 'model outside a delegated turn'
        steps = self.turns[turn]
        native = steps[0].get('model', 'gpt-5.4') != 'gpt-5.4'
        assert body['model'] == steps[0].get('model', 'gpt-5.4'), 'wrong delegated model'
        tools = self.native_tools(body) if native else body['tools']
        assert tools == function_tools([]), 'session catalog mismatch'
        assert self.step < len(steps), 'unexpected model retry'
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output']
        mine = [item for item in outputs if item['call_id'].startswith(f'session-{turn}-')]
        assert len(mine) == self.step, 'tool output count mismatch'
        for position, item in enumerate(mine):
            assert item['call_id'] == f'session-{turn}-{position}', 'tool identity mismatch'
        step = steps[self.step]
        if 'request' in steps[0]:
            # The pinned app server gives the runner the user's words and the
            # transcript, never a request the voice model wrote.
            request = steps[0]['request']
            instructions, delegated = body.get('instructions', ''), json.dumps(body['input'])
            if native or 'instructions' not in body:
                # The base instructions are the first developer message and the
                # hand-off is the last user message: always for Responses Lite,
                # and for every model since Codex stopped sending `instructions`
                # (observed at rust-v0.171.0).
                messages = [item for item in body['input'] if item.get('type') == 'message']
                assert [messages[0]['role'], messages[-1]['role']] == ['developer', 'user'], 'native input shape'
                instructions, delegated = messages[0]['content'][0]['text'], json.dumps(messages[-1])
            assert ('If those latest words only say the user is finished' in instructions
                    and 'immediately call end_session with {} once' in instructions), \
                'missing goodbye runner instructions'
            assert f'<input>{request}</input>' in delegated and f'user: {request}' in delegated, \
                'runner did not receive the user words'
            assert 'end the session' not in delegated, 'synthetic end request reached the runner'
        if step.get('wait_for_release'):
            emit({'event': 'provider.waiting', 'turn': turn})
            assert release_goodbye.wait(20), 'runner was not released at the scripted boundary'
            release_goodbye.clear()
        if 'call' in step:
            item = {'type': 'function_call', 'id': f'fc-{turn}-{self.step}', 'call_id': f'session-{turn}-{self.step}',
                    'name': step['call'], 'arguments': json.dumps(step['arguments']), 'status': 'completed'}
            if native:
                item['namespace'] = 'functions'
        elif 'say' in step:
            item = {'type': 'message', 'id': f'msg-{turn}', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': step['say'], 'annotations': []}]}
        else:
            assert step == {'silent': True}, 'invalid silent handback'
            item = None
        self.step += 1
        if 'heard' in step:
            # The turn is in flight: this model request is still unanswered.
            commands.put({'heard': step['heard']})
            wake_write.sendall(b'e')
            assert heard.wait(20), 'sideband did not deliver the overlapping speech'
        observed['models'].append({'turn': turn, 'tools': [tool['name'] for tool in tools[:-1]]})
        emit({'event': 'provider.model', 'turn': turn, 'outputs': [item['output'] for item in mine]})
        response = {'id': f'response-{turn}-{self.step}', 'status': 'completed', 'output': [item] if item else []}
        events = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}}]
        if item:
            events.append({'type': 'response.output_item.done', 'output_index': 0, 'item': item})
        events.append({'type': 'response.completed', 'response': response})
        return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events).encode()

    @staticmethod
    def native_tools(body):
        """The session catalog a code-mode-only model can call without `exec`.

        The Echo has no code-mode host, so a tool that is only an `exec`
        callable cannot be called there. Responses Lite carries declarations
        as one input block of namespaces; search is the app server's `web.run`.

        No release is pinned, so the app server's own tools are not compared:
        the T3 native-extras witness shows they do nothing in a bridge thread.
        Every tool the bridge declares must be a direct declaration, in order."""
        assert 'tools' not in body, 'unexpected conventional tools field'
        blocks = [item['tools'] for item in body['input'] if 'tools' in item]
        assert len(blocks) == 1, 'missing or repeated native tool block'
        names = [namespace['name'] + '.' + tool['name'] for namespace in blocks[0] for tool in namespace['tools']]
        declared = [tool['name'] for tool in function_tools([])[:-1]]
        direct = [tool for namespace in blocks[0] if namespace['name'] == 'functions'
                  for tool in namespace['tools'] if tool['name'] in declared]
        assert [tool['name'] for tool in direct] == declared and 'web.run' in names, \
            'bridge tools are not direct declarations: ' + json.dumps(names)
        return direct + function_tools([])[-1:]

    def begin(self, turn, request=None):
        assert turn == len(observed['delegations']), 'unexpected delegation identity'
        if turn:
            assert self.step == len(self.turns[turn - 1]), 'overlapping delegation'
            prior = self.turns[turn - 1][-1]
            if prior.get('abandoned'):
                assert f'session-turn-{turn - 1}' not in observed['appends'], 'abandoned turn had a handback'
            elif 'say' in prior:
                assert f'session-turn-{turn - 1}' in observed['appends'], 'ordinary handback is missing'
        self.step = 0
        name = f'session-turn-{turn}'
        observed['delegations'].append(name)
        return {'type': 'delegation.created', 'item': {
            'type': 'delegation', 'target': 'client', 'id': name,
            'content': [{'type': 'input_text', 'text': request or self.turns[turn][0].get(
                'request', f'Answer session turn {turn}.')}]}}

    def append(self, value):
        turn = self.turn()
        assert self.step == len(self.turns[turn]), 'append before the scripted answer'
        assert value['delegation_item_id'] == f'session-turn-{turn}', 'append identity mismatch'
        text = self.turns[turn][-1].get('say', '')
        assert value['content'] == [{'type': 'input_text', 'text': text}], 'append content mismatch'
        observed['appends'].append(value['delegation_item_id'])

    def transcripts(self, turn):
        values = []
        for role, text in [('user', 'ping'), ('assistant', self.turns[turn][-1]['say'])]:
            kind = 'input' if role == 'user' else 'output'
            values.append({'type': kind + '_transcript.added', 'item': {'text': text}})
            values.append({'type': 'turn.done', 'turn': {'role': role, 'transcript': text}})
        return values


def time_reply(instructions, question, latest=None):
    """Model a direct answer from Live's current time context."""
    base, _, context = instructions.rpartition('\n')
    if latest is not None:
        context = latest.splitlines()[0]
    match = re.fullmatch(r'\[(?:Call-start|Wake-time) context: (\d{4}-\d{2}-\d{2}) ([A-Za-z]+) '
                         r'(\d{2}:\d{2}:\d{2}) UTC\+00:00; (.+)\]', context)
    assert match, 'missing or malformed call-start context'
    date, weekday, time, suffix = match.groups()
    utc = datetime.fromisoformat(f'{date}T{time}').replace(tzinfo=timezone.utc)
    assert utc.strftime('%A') == weekday, 'wrong UTC weekday'
    zone = 'America/Los_Angeles'
    if suffix == f'operator time zone: {zone}; answer time questions in {zone}':
        local = utc.astimezone(ZoneInfo(zone))
        replies = {
            'What time is it?': local.strftime('%H:%M'),
            "What is today's date?": local.date().isoformat(),
            'What day of the week is it?': local.strftime('%A'),
        }
        assert SEARCH_GUIDANCE in base, 'expected the installed-style example voice prompt'
    else:
        assert suffix == 'local offset unavailable', 'unexpected time zone context'
        assert SEARCH_GUIDANCE not in base, 'expected the built-in voice prompt'
        replies = {'What time is it in my time zone?':
                   f"{utc:%H:%M} UTC. I don't know your local time zone."}
    assert question in replies, 'unexpected question for the received time context'
    if TIME_RULE not in base:
        return context, None
    assert UTC_CAVEAT in base, 'missing conditional UTC caveat'
    return context, replies[question]


def send_delegation(connection, value):
    frame(connection, value)
    emit({'event': 'provider.delegation', 'delegation_id': value['item']['id'],
          'request': value['item']['content'][0]['text']})


def answer_time(connection, question, media=False):
    instructions = script.initial_history if media else script.instructions
    latest = next((row['item']['content'][0]['text'] for row in reversed(observed['time_wire'])
                   if row['item'].get('type') == 'session.context.append'), None) if media else None
    context, answer = time_reply(instructions, question, latest)
    decision = {'event': 'provider.time_turn', 'question': question, 'context': context, 'answer': answer}
    if media:
        decision.update(trigger='uplink_audio', order=time_order())
        observed['media_time_turns'].append(decision)
    frame(connection, {'type': 'turn.done', 'turn': {'role': 'user', 'transcript': question}})
    if answer is None:
        send_delegation(connection, script.begin(0, question))
    else:
        frame(connection, {'type': 'output_transcript.added', 'item': {'text': answer}})
        if media:
            observed['media_time_transcripts'].append({'question_order': decision['order'], 'text': answer})
    emit(decision)


def time_order():
    return len(observed['time_wire']) + len(observed['media_time_turns']) + 1


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
            observed['model_requests'] += 1
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
        session = body.get('session', {})
        script.instructions = session.get('instructions', '')
        history = session.get('initial_items') or []
        script.initial_history = next((part['text'] for item in history
                                       if item.get('type') == 'message' and item.get('role') == 'developer'
                                       for part in item['content'] if part.get('type') == 'input_text'), '')
        if script and any('request' in turn[0] for turn in script.turns):
            if 'hand off once so the background agent ends the session' not in script.instructions:
                emit({'event': 'provider.failure', 'failure': 'missing goodbye Live prompt'})
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
                        else:
                            observed['time_wire'].append({'item': value, 'order': time_order()})
                if wake_read in readable:
                    assert wake_read.recv(1) == b'e'
                    command = commands.get_nowait()
                    if 'delegate' in command:
                        send_delegation(self.connection, script.begin(command['delegate']))
                    elif 'time_question' in command:
                        answer_time(self.connection, command['time_question'])
                    elif 'media_question' in command:
                        answer_time(self.connection, command['media_question'], media=True)
                    elif 'events' in command:
                        for value in command['events']:
                            frame(self.connection, value)
                        emit({'event': 'provider.events_sent', 'tag': command['tag']})
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
        except (AssertionError, ValueError) as error:
            emit({'event': 'provider.failure', 'failure': str(error)})
        self.close_connection = True


def main():
    global script
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
            elif any(key in command for key in ('delegate', 'transcripts', 'events', 'time_question', 'media_question')):
                commands.put(command)
                wake_write.sendall(b'e')
            elif command == {'release_goodbye': True}:
                release_goodbye.set()
            elif command == {'snapshot': True}:
                emit({'event': 'provider.snapshot', **observed})
            else:
                raise AssertionError('unknown fixture command')
    finally:
        server.shutdown()
        server.server_close()
        wake_read.close()
        wake_write.close()


if __name__ == '__main__':
    main()
