"""Use the session sim's V3 sideband with a wire-driven model and fake MCP."""
import http.server
import json
from pathlib import Path
import socket
import sys
import threading
import urllib.request

from catalog import intent

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import provider as session

MODEL = 'gpt-6.1-sol'
OFFER = 'v=offer\r\n'
ANSWER = 'v=answer\r\n'


class Turn:
    def __init__(self, case, variant, fault=None):
        self.case = case
        self.variant = variant
        self.fault = fault
        self.id = f'{variant}-{case.id}-{fault or "ok"}'
        self.raw = []
        self.calls = []
        self.selected = None
        self.diagnostic = None
        self.answer = None
        self.appended = False
        self.sideband_closed = threading.Event()

    @property
    def call_id(self):
        return f'{self.id}-call'

    def begin(self, index):
        assert index == 0
        return {'type': 'delegation.created', 'item': {'type': 'delegation', 'target': 'client',
                'id': self.id, 'content': [{'type': 'input_text', 'text': self.case.utterance}]}}

    def append(self, value):
        assert not self.appended and value['delegation_item_id'] == self.id
        assert value['content'] == [{'type': 'input_text', 'text': self.answer}]
        self.appended = True

    def respond(self, raw):
        body = json.loads(raw)
        index = len(self.raw)
        self.raw.append(raw)
        assert body['model'] == MODEL and 'tools' not in body, 'expected Responses Lite model'
        utterance = self.utterance(body['input'])
        if self.fault == 'peer-close':
            self.diagnostic = 'model peer closed while request was pending'
            return None
        item = self.next_item(body['input'], utterance, index)
        response = {'id': f'{self.id}-response-{index}', 'status': 'completed', 'output': [item]}
        events = [{'type': 'response.created', 'response': {**response, 'status': 'in_progress', 'output': []}},
                  {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                  {'type': 'response.completed', 'response': response}]
        return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n'
                       for event in events).encode()

    def utterance(self, items):
        matches = []
        for item in items:
            if item.get('role') == 'user':
                for part in item['content']:
                    for text in part.get('text', '').splitlines():
                        if self.case.utterance in text:
                            matches.append(self.case.utterance)
        assert matches, 'delegation did not carry the fixed utterance'
        return matches[0]

    def next_item(self, items, utterance, index):
        catalog, = [item['tools'] for item in items if item['type'] == 'additional_tools']
        search = [tool for tool in catalog if tool.get('type') == 'tool_search']
        if self.variant == 'deferred':
            assert len(search) == 1 and search[0]['execution'] == 'client', 'client search is missing'
            assert not any(name.startswith('portos_') for name, _ in functions(catalog)), 'deferred tool leaked upfront'
            if index == 0:
                return {'type': 'tool_search_call', 'id': f'{self.id}-search-item', 'status': 'completed',
                        'execution': 'client', 'call_id': f'{self.id}-search',
                        'arguments': {'query': intent(utterance), 'limit': 1}}
            loaded, = [item for item in items if item['type'] == 'tool_search_output']
            assert loaded['call_id'] == f'{self.id}-search' and loaded['execution'] == 'client'
            catalog = loaded['tools']
        else:
            assert not search, 'direct replay advertised search'
        if self.selected:
            outputs = [item['output'] for item in items if item['type'] == 'function_call_output'
                       and item['call_id'] == self.call_id]
            assert len(outputs) == 1 and f'receipt {self.call_id}' in json.dumps(outputs[0]), 'wrong MCP receipt'
            self.answer = 'The simulated tool completed.'
        else:
            selected = [(namespace, name) for namespace, name in functions(catalog) if name == intent(utterance)]
            if selected:
                self.selected, = selected
                return {'type': 'function_call', 'id': f'{self.id}-function', 'call_id': self.call_id,
                        'namespace': self.selected[0], 'name': self.selected[1], 'arguments': '{}', 'status': 'completed'}
            self.diagnostic = f'missing reachable candidate {intent(utterance)}'
            self.answer = 'The simulated tool is unavailable.'
        return {'type': 'message', 'id': f'{self.id}-answer', 'role': 'assistant', 'status': 'completed',
                'content': [{'type': 'output_text', 'text': self.answer, 'annotations': []}]}

    def dispatch(self, origin, request, thread):
        assert self.fault is None and self.selected, 'unexpected dynamic tool call'
        params = request['params']
        assert params['threadId'] == thread and params['callId'] == self.call_id
        assert (params.get('namespace') or 'functions', params['tool']) == self.selected
        mcp = {'jsonrpc': '2.0', 'id': self.call_id, 'method': 'tools/call',
               'params': {'name': params['tool'], 'arguments': params['arguments']}}
        endpoint = urllib.request.Request(f'{origin}/mcp', json.dumps(mcp).encode(),
                                           {'Content-Type': 'application/json'})
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(endpoint, timeout=3) as response:
            reply = json.load(response)
        assert reply['id'] == self.call_id
        content = reply['result']['content']
        assert len(content) == 1 and content[0]['type'] == 'text'
        return {'id': request['id'], 'result': {'success': True, 'contentItems': [
            {'type': 'inputText', 'text': content[0]['text']}]}}


def functions(tools):
    for spec in tools:
        if spec['type'] == 'namespace':
            for tool in spec['tools']:
                if tool['type'] == 'function':
                    yield spec['name'], tool['name']
        elif spec['type'] == 'function':
            yield 'functions', spec['name']


class Handler(session.Handler):
    def do_POST(self):
        turn = session.script
        raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        try:
            if self.path == '/v1/responses':
                answer = turn.respond(raw)
                if answer is None:
                    self.close_connection = True
                    self.connection.shutdown(socket.SHUT_RDWR)
                else:
                    self.respond(200, answer, 'text/event-stream')
            elif self.path == '/mcp':
                value = json.loads(raw)
                assert value['method'] == 'tools/call' and value['id'] == turn.call_id
                assert value['params'] == {'name': turn.selected[1], 'arguments': {}}
                turn.calls.append(value['params']['name'])
                result = {'id': value['id'], 'jsonrpc': '2.0', 'result': {'content': [
                    {'type': 'text', 'text': f'receipt {value["id"]}'}]}}
                self.respond(200, json.dumps(result).encode())
            elif self.path.startswith('/backend-api/codex/realtime/calls?'):
                assert json.loads(raw)['sdp'] == OFFER
                self.respond(201, ANSWER.encode(), 'application/sdp', '/v1/live/rtc_local')
            else:
                session.observed['external'] += 1
                self.respond(403, b'{}')
        except (AssertionError, KeyError, ValueError) as error:
            reason = str(error) if isinstance(error, AssertionError) else type(error).__name__
            self.server.failures.append(reason)
            self.respond(400, b'{"error":{"message":"catalog peer rejected request"}}')

    def do_GET(self):
        turn = session.script
        try:
            super().do_GET()
        finally:
            turn.sideband_closed.set()


class Peers(http.server.ThreadingHTTPServer):
    def __init__(self):
        super().__init__(('127.0.0.1', 0), Handler)
        self.origin = f'http://127.0.0.1:{self.server_port}'
        self.failures = []
        self.task = threading.Thread(target=self.serve_forever, kwargs={'poll_interval': 0.05})

    def __enter__(self):
        self.task.start()
        return self

    def __exit__(self, *args):
        self.shutdown()
        self.server_close()
        self.task.join()
        session.wake_read.close()
        session.wake_write.close()

    def delegate(self):
        session.commands.put({'delegate': 0})
        session.wake_write.sendall(b'e')
