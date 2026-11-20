"""Local Responses/delegation observer; emits no arbitrary model input or secrets."""
import json
import struct
import re
import sys
import threading
from copy import deepcopy


def exact(connection, count):
    result = b''
    while len(result) < count:
        chunk = connection.recv(count - len(result))
        if not chunk:
            raise EOFError()
        result += chunk
    return result


def read_frame(connection):
    first, second = exact(connection, 2)
    length = second & 127
    if length == 126:
        length = struct.unpack('!H', exact(connection, 2))[0]
    elif length == 127:
        length = struct.unpack('!Q', exact(connection, 8))[0]
    assert length <= 1048576, 'oversized sideband frame'
    mask = exact(connection, 4) if second & 128 else b''
    data = exact(connection, length)
    if mask:
        data = bytes(value ^ mask[index % 4] for index, value in enumerate(data))
    return first & 15, data


class Delegation:
    def __init__(self, tools, negative, exhausted=False, model="gpt-5.4", web_search="live",
                 identity=None, history=()):
        self.tools = tools
        self.web_search = web_search
        self.model = model
        self.native = model != "gpt-5.4"
        self.negative = negative
        self.exhausted = exhausted
        self.reply = "I couldn't call test echo." if negative else 'test echo: delegated echo'
        self.models = []
        self.delegations = []
        self.appends = []
        self.append_receipts = []
        self.identity = identity or 'delegation-local'
        self.history = list(history)
        self.outputs = []
        self.call_id = identity + '-tool' if identity else 'tool-local'
        self.prompt = 'Call test_echo with text delegated echo and report the result.'
        self.release = threading.Event() if identity and exhausted else None
        self.ingress = []
        self.lock = threading.Lock()
        if identity:
            self.prompt = f'Fixture delegation {identity}. {self.prompt}'

    def begin(self):
        assert not self.delegations, 'duplicate delegation'
        self.delegations.append(self.identity)
        return {'type': 'delegation.created', 'item': {
            'type': 'delegation', 'target': 'client', 'id': self.identity,
            'content': [{'type': 'input_text', 'text': self.prompt}]}}

    def accept_model(self, body):
        with self.lock:
            if self.release is not None:
                assert not self.release.is_set(), 'model ingress after interrupted turn'
                assert not self.ingress, 'repeated model ingress for interrupted turn'
                self.ingress.append(self.identity)
            return self.respond(body)

    def release_model(self):
        with self.lock:
            assert self.release is not None and not self.release.is_set(), 'unexpected model release'
            self.release.set()

    def respond(self, body):
        expected = [{'type': 'function', 'name': tool['name'], 'description': tool['description'],
                     # rust-v0.160.0 closes each dynamic object schema on the wire.
                     'parameters': {**deepcopy(tool['inputSchema']), 'additionalProperties': False},
                     'strict': False} for tool in self.tools]
        # The same pin drops numeric minimum while lowering JSON schema to its
        # tool-schema type. Assert that measured representation exactly.
        expected[1]['parameters']['properties']['ms'].pop('minimum')
        for tool in expected:
            if tool['name'] == 'extend_session':
                tool['parameters']['properties']['seconds'].pop('minimum')
        names = self.check_tools(body, expected)
        assert body['model'] == self.model, 'wrong delegated model'
        outputs = [item for item in body['input'] if item.get('type') == ('custom_tool_call_output' if self.native else 'function_call_output')]
        assert outputs[:len(self.history)] == self.history, 'prior tool outputs changed across turns'
        outputs = outputs[len(self.history):]
        index = len(self.models)
        assert index < 2, 'unexpected model retry'
        if index == 0:
            assert not outputs, 'premature tool output'
            item = {'type': 'function_call', 'id': 'fc-local', 'call_id': self.call_id,
                    'name': 'test_echo', 'arguments': '{"text":"delegated echo"}', 'status': 'completed'}
            if self.native:
                item = {'type': 'custom_tool_call', 'id': 'fc-local', 'call_id': self.call_id,
                        'name': 'exec', 'namespace': 'functions',
                        'input': 'text(await tools.test_echo({text: "delegated echo"}));', 'status': 'completed'}
        else:
            assert len(outputs) == 1 and outputs[0]['call_id'] == self.call_id, 'tool identity mismatch'
            # The pinned engine encodes dynamic-tool content as a JSON text value.
            self.check_output(outputs[0]['output'])
            self.outputs = deepcopy(outputs)
            item = {'type': 'message', 'id': 'msg-local', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': self.reply, 'annotations': []}]}
        self.models.append({'tools': names, 'outputs': len(outputs)})
        response = {'id': f'response-{index}', 'status': 'completed', 'output': [item]}
        events = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}},
                  {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                  {'type': 'response.completed', 'response': response}]
        return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events).encode()

    def check_tools(self, body, expected):
        hosted = []
        if self.web_search != 'disabled':
            search = {'type': 'web_search', 'external_web_access': self.web_search == 'live'}
            hosted = [search]
        if not self.native:
            assert body['tools'] == expected + hosted, 'model exposed tools beyond exact dynamic catalog: ' + json.dumps(body['tools'])
            return [tool['name'] for tool in expected]
        assert 'tools' not in body, 'unexpected conventional native tools field'
        assert not any(item.get('additional_tools') for item in body['input']), 'unexpected hosted native tools'
        # Bundled sol uses Responses Lite: declarations are input tool blocks.
        blocks = [item['tools'] for item in body['input'] if 'tools' in item]
        assert len(blocks) == 1, 'missing or repeated native tool block'
        namespaces = blocks[0]
        # Responses Lite uses the pin's direct provider-backed web.run extension.
        # It never becomes an exec nested callable or a hosted declaration.
        names = [namespace['name'] + '.' + tool['name']
                 for namespace in namespaces for tool in namespace['tools']]
        description = namespaces[0]['tools'][0]['description']
        nested = re.findall(r'^### `([^`]+)`$', description, re.M)
        print(json.dumps({'model': self.model, 'tools': names, 'nested': nested}), file=sys.stderr, flush=True)
        # Pin spec_plan.rs:1178-1204 registers async input from bundled model
        # metadata independently of tools.experimental_request_user_input.enabled.
        # :1225-1251 registers clock from that metadata and default sleep_tool.
        # The binding amendment permits these exact extras, not synchronous input.
        expected_names = ['functions.exec', 'functions.wait', 'functions.request_user_input_async', 'clock.sleep']
        if self.web_search != 'disabled':
            expected_names += ['web.run']
        assert names == expected_names, names
        assert nested == sorted(tool['name'] for tool in self.tools) + ['clock__curr_time'], nested
        for tool, fields in zip(self.tools, ['text: string;', 'ms: number;', '', 'name: string;']):
            declaration = 'declare const tools: { ' + tool['name'] + '(args: { ' + fields + ' }): Promise<unknown>; };'
            if not fields:
                declaration = declaration.replace('{  }', '{}')
            assert declaration in description, 'native schema mismatch: ' + declaration
            assert tool['description'] in description, 'native description changed'
        return names

    def check_output(self, output):
        if self.native:
            # Observe the entire synthetic result before decoding, including a
            # runtime failure or unexpected platform-specific representation.
            print(json.dumps({'native_raw_output': output}), file=sys.stderr, flush=True)
            assert isinstance(output, list) and len(output) == 2, 'native output content count mismatch: ' + repr(output)
            assert output[0]['type'] == 'input_text'
            assert re.fullmatch(r'Script completed\nWall time [0-9.]+ seconds\nOutput:\n', output[0]['text']), 'native execution did not complete'
            assert output[1]['type'] == 'input_text'
            output = output[1]['text']
        if self.native:
            print(json.dumps({'native_tool_output': output}), file=sys.stderr, flush=True)
        if self.negative:
            assert output == 'tool execution failed', 'exact failure output mismatch'
        else:
            assert json.loads(output) == [{'type': 'text', 'text': 'delegated echo'}], 'exact tool output mismatch'

    def append(self, value):
        if self.exhausted:
            raise AssertionError('interrupted turn appended a false result')
        assert not self.appends, 'duplicate append'
        assert len(self.models) == 2, 'append before verified output'
        assert value['delegation_item_id'] == self.identity, 'wrong delegation correlation'
        assert value['content'] == [{'type': 'input_text', 'text': self.reply}], 'wrong appended result'
        self.append_receipts.append(deepcopy(value))
        self.appends.append(value['delegation_item_id'])

    def snapshot(self):
        if self.native:
            print(json.dumps({'models': self.models, 'correlated_appends': self.appends}), file=sys.stderr, flush=True)
        return {'delegations': self.delegations, 'models': self.models, 'appends': self.appends}


def model_identity(body):
    # The pin includes earlier turns in Responses input. The latest marked
    # delegation owns this request, even if the fixture has started another one.
    for item in reversed(body['input']):
        if item.get('role') != 'user':
            continue
        for content in reversed(item['content']):
            match = re.search(r'<input>Fixture delegation ([a-z-]+)\.', content.get('text', ''))
            if match:
                return match[1]
    raise AssertionError('missing fixture delegation in Responses input')
