"""Local Responses/delegation observer; emits no arbitrary model input or secrets."""
import json
import struct
import re
import sys
import threading
from copy import deepcopy

# Built-in tools the bridge disables in every thread (echo-codex ThreadSettings):
# command execution, images, planning, synchronous user input and sub-agents.
SWITCHED_OFF = ('shell', 'shell_command', 'local_shell', 'exec_command', 'unified_exec', 'write_stdin',
                'view_image', 'update_plan', 'request_user_input', 'spawn_agent', 'send_message',
                'wait_agent', 'close_agent', 'resume_agent')


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


class Reserved(AssertionError):
    """The backend's refusal of a client declaration that shadows a model-reserved tool."""

    def __init__(self, name):
        super().__init__(f"Function '{name}' is reserved for use by this model and must match the configured schema.")
        self.name = name


def reserves_web_run(model):
    """gpt-6.2-* reserve web.run and the live backend refuses a direct
    declaration of it (HTTP 400, measured with Codex 0.171.0, 2026-11-21).
    gpt-6.1-sol, the bridge's default delegated model, accepts it and searches."""
    return model.startswith('gpt-6.2-')


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
        self.probes = {}
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

    def respond_catalog(self, body):
        expected = [{'type': 'function', 'name': tool['name'], 'description': tool['description'],
                     # The app server closes each dynamic object schema on the wire.
                     'parameters': {**deepcopy(tool['inputSchema']), 'additionalProperties': False},
                     'strict': False} for tool in self.tools]
        # It also drops numeric minimum while lowering JSON schema to its
        # tool-schema type. Assert that measured representation exactly.
        expected[1]['parameters']['properties']['ms'].pop('minimum')
        for tool in expected:
            if tool['name'] == 'extend_session':
                tool['parameters']['properties']['seconds'].pop('minimum')
        return self.check_tools(body, expected)

    def respond(self, body):
        names = self.respond_catalog(body)
        assert body['model'] == self.model, 'wrong delegated model'
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output'
                   and item['call_id'] not in self.probes]
        assert outputs[:len(self.history)] == self.history, 'prior tool outputs changed across turns'
        outputs = outputs[len(self.history):]
        index = len(self.models)
        assert index < 2, 'unexpected model retry'
        if index == 0:
            assert not outputs, 'premature tool output'
            item = {'type': 'function_call', 'id': 'fc-local', 'call_id': self.call_id,
                    'name': 'test_echo', 'arguments': '{"text":"delegated echo"}', 'status': 'completed'}
            if self.native:
                # A code-mode-only model calls the bridge's tool directly; `exec`
                # needs a code-mode host, which the Echo does not have.
                item['namespace'] = 'functions'
        else:
            assert len(outputs) == 1 and outputs[0]['call_id'] == self.call_id, 'tool identity mismatch'
            # The app server encodes dynamic-tool content as a JSON text value.
            self.check_output(outputs[0]['output'])
            self.outputs = deepcopy(outputs)
            item = {'type': 'message', 'id': 'msg-local', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': self.reply, 'annotations': []}]}
        self.models.append({'tools': names, 'outputs': len(outputs)})
        return stream(f'response-{index}', [item])

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
        # The bridge's catalog and, while search is on, the app server's
        # web.run extension are declared directly: web.run is the only search
        # path for a code-mode-only model on the Echo, which has no code-mode
        # host for `exec`. A model that reserves web.run refuses the turn.
        names = [namespace['name'] + '.' + tool['name']
                 for namespace in namespaces for tool in namespace['tools']]
        description = namespaces[0]['tools'][0]['description']
        nested = re.findall(r'^### `([^`]+)`$', description, re.M)
        print(json.dumps({'model': self.model, 'tools': names, 'nested': nested}), file=sys.stderr, flush=True)
        # No release is pinned, so the app server's own tools are not compared
        # exactly (owner, 2026-11-21). Every tool the bridge declares must be a
        # direct declaration, exactly as a conventional model sees it and in
        # order: `exec` needs a code-mode host the Echo does not have.
        declared = ['functions.' + tool['name'] for tool in expected]
        functions = [tool for namespace in namespaces if namespace['name'] == 'functions'
                     for tool in namespace['tools']]
        direct = [tool for tool in functions if 'functions.' + tool['name'] in declared]
        assert direct == expected, 'native direct catalog mismatch: ' + json.dumps(direct)
        if 'web.run' in names and reserves_web_run(self.model):
            raise Reserved('web.run')
        # Search follows the bridge's own setting in both directions.
        assert ('web.run' in names) == (self.web_search != 'disabled'), 'direct search mismatch: ' + json.dumps(names)
        # What the bridge switches off stays off, whatever else a release adds.
        refused = sorted(set(names) & {'functions.' + name for name in SWITCHED_OFF})
        assert not refused, 'a tool the bridge switches off reached the model: ' + json.dumps(refused)
        self.native_catalog = {'tools': names, 'nested': nested,
                               'extras': [name for name in names if name not in declared + ['web.run']]}
        self.native_declarations = {namespace['name'] + '.' + tool['name']: tool
                                    for namespace in namespaces for tool in namespace['tools']}
        return declared + (['web.run'] if self.web_search != 'disabled' else [])

    def check_output(self, output):
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


class RunnerSkills(Delegation):
    names = ('brief', 'unused', 'unlisted', 'malformed')
    expected = ({'body': 'Summarize the answer in one sentence.\n'},
                {'error': 'skill_not_enabled'}, {'error': 'skill_not_enabled'}, {'error': 'invalid_arguments'})

    mcp_content = [{'type': 'text', 'text': 'MCP delivered brief.'}]

    def __init__(self, tools, mcp):
        super().__init__(tools, False, model='gpt-6.1-sol', web_search='disabled', identity='runner-skills')
        self.prompt = 'Fixture delegation runner-skills. Check brief, unused, unlisted, and malformed arguments.'
        self.reply = 'Finished checking the enabled skill.'
        self.mcp = mcp
        self.mcp_calls = []
        if mcp:
            self.names = ('brief', 'malformed')
            self.expected = (self.mcp_content, 'tool request refused')
            self.prompt = 'Fixture delegation runner-skills. Try the configured MCP reader and malformed arguments.'
            self.reply = 'Finished checking the MCP reader.'

    def mcp_response(self, request):
        assert self.mcp is not None and request['jsonrpc'] == '2.0'
        method = request['method']
        if method == 'notifications/initialized':
            return 202, None
        if method == 'initialize':
            result = {'protocolVersion': '2025-03-26', 'capabilities': {'tools': {}},
                      'serverInfo': {'name': 'skill-name-fixture', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{**self.tools[0], 'name': self.mcp}]}
        else:
            assert method == 'tools/call'
            self.mcp_calls.append(request['params'])
            result = {'content': self.mcp_content}
        return 200, {'jsonrpc': '2.0', 'id': request['id'], 'result': result}

    def respond(self, body):
        assert body['model'] == self.model
        expected = [{'type': 'function', 'name': tool['name'], 'description': tool['description'],
                     'parameters': deepcopy(tool['inputSchema']), 'strict': False} for tool in self.tools]
        for tool in expected:
            if tool['name'] == 'extend_session':
                tool['parameters']['properties']['seconds'].pop('minimum')
        tools = self.check_tools(body, expected)
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output']
        index = len(self.models)
        assert index <= len(self.names) and len(outputs) == index, 'wrong skill output count'
        for name, item, result in zip(self.names, outputs, self.expected):
            assert item['call_id'] == f'skill-{name}', 'wrong skill call ID'
            actual = item['output'] if isinstance(result, str) else json.loads(item['output'])
            assert actual == result, 'wrong skill result: ' + json.dumps(item)
        self.outputs = outputs
        if index < len(self.names):
            name = self.names[index]
            arguments = [] if name == 'malformed' else {'name': name}
            item = {'type': 'function_call', 'id': f'fc-{name}', 'call_id': f'skill-{name}',
                    'namespace': 'functions', 'name': 'read_skill', 'arguments': json.dumps(arguments),
                    'status': 'completed'}
        else:
            item = {'type': 'message', 'id': 'msg-skills', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': self.reply, 'annotations': []}]}
        self.models.append({'tools': tools, 'outputs': index})
        return stream(f'skills-{index}', [item])

    def append(self, value):
        assert not self.appends and len(self.models) == len(self.names) + 1, 'unexpected skill append'
        assert value['delegation_item_id'] == self.identity
        assert value['content'] == [{'type': 'input_text', 'text': self.reply}]
        self.appends.append(self.identity)

    def snapshot(self):
        return {**super().snapshot(), 'skill_outputs': self.outputs, 'mcp_calls': self.mcp_calls}


class EndCall(Delegation):
    """The user asks to end the call: the delegated model calls end_session once,
    directly and with no arguments, then reports the farewell it was handed."""
    def __init__(self, tools):
        super().__init__(tools, False, model='gpt-6.1-sol', web_search='disabled', identity='end-call')
        self.call_id = 'end-call-end_session'
        self.prompt = 'Fixture delegation end-call. The user said: thanks, that is all, please end the call now.'
        self.reply = 'Ending the call now.'
        # The turn keeps running after end_session's result until released.
        self.held = threading.Event()

    def respond(self, body):
        assert body['model'] == self.model
        expected = [{'type': 'function', 'name': tool['name'], 'description': tool['description'],
                     'parameters': deepcopy(tool['inputSchema']), 'strict': False} for tool in self.tools]
        for tool in expected:
            if tool['name'] == 'extend_session':
                tool['parameters']['properties']['seconds'].pop('minimum')
        tools = self.check_tools(body, expected)
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output']
        index = len(self.models)
        assert index < 2 and len(outputs) == index, 'wrong end_session output count'
        self.outputs = deepcopy(outputs)
        if index == 0:
            item = {'type': 'function_call', 'id': 'fc-end', 'call_id': self.call_id, 'namespace': 'functions',
                    'name': 'end_session', 'arguments': '{}', 'status': 'completed'}
        else:
            assert outputs[0]['call_id'] == self.call_id, 'wrong end_session call ID'
            item = {'type': 'message', 'id': 'msg-end', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': self.reply, 'annotations': []}]}
        self.models.append({'tools': tools, 'outputs': index})
        return stream(f'end-call-{index}', [item])

    def append(self, value):
        assert not self.appends and len(self.models) == 2, 'unexpected end-call append'
        assert value['delegation_item_id'] == self.identity
        assert value['content'] == [{'type': 'input_text', 'text': self.reply}]
        self.appends.append(self.identity)

    def snapshot(self):
        return {**super().snapshot(), 'end_outputs': self.outputs}


class NativeExtras(Delegation):
    """The app server's own tools beyond the bridge's catalog are called once each.

    No release is pinned, so those tools are not refused by name. What holds for
    any release is that they do nothing here: the thread's sandbox is read-only
    and its configuration names no MCP server."""
    patch = '*** Begin Patch\n*** Add File: {}\n+written by a native tool\n*** End Patch\n'
    arguments = {'list_mcp_resources': {}, 'list_mcp_resource_templates': {},
                 'read_mcp_resource': {'server': 'bridge', 'uri': 'file:///native-extra'}}

    def __init__(self, tools, target):
        super().__init__(tools, False, model='gpt-6.1-sol')
        self.target = target
        self.results = None

    def respond(self, body):
        if self.results is not None:
            return super().respond(body)
        if not self.probes:
            return self.probe(body)
        kinds = ('function_call_output', 'custom_tool_call_output')
        outputs = {item['call_id']: item['output'] for item in body['input']
                   if item.get('type') in kinds and item['call_id'] in self.probes}
        assert sorted(outputs) == sorted(self.probes), 'a native extra returned no output: ' + json.dumps(outputs)
        self.results = {self.probes[call]: output for call, output in outputs.items()}
        return super().respond(body)

    def probe(self, body):
        self.respond_catalog(body)
        items = []
        for name in self.native_catalog['extras']:
            namespace, _, tool = name.partition('.')
            declaration = self.native_declarations[name]
            call = {'id': 'probe-item-' + tool, 'call_id': 'probe-' + tool, 'namespace': namespace,
                    'name': tool, 'status': 'completed'}
            if tool == 'apply_patch':
                assert declaration['type'] == 'custom', 'apply_patch is no longer a freeform tool'
                items.append({**call, 'type': 'custom_tool_call', 'input': self.patch.format(self.target)})
            elif tool in self.arguments:
                items.append({**call, 'type': 'function_call', 'arguments': json.dumps(self.arguments[tool])})
            else:
                continue
            self.probes[call['call_id']] = name
        return stream('probe', items)

    def snapshot(self):
        return {**super().snapshot(), 'catalog': self.native_catalog, 'extras': self.results}


def stream(identity, items):
    response = {'id': identity, 'status': 'completed', 'output': items}
    events = [{'type': 'response.created', 'response': {'id': identity, 'status': 'in_progress', 'output': []}}]
    events += [{'type': 'response.output_item.done', 'output_index': index, 'item': item}
               for index, item in enumerate(items)]
    events.append({'type': 'response.completed', 'response': response})
    return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events).encode()


def model_identity(body):
    # The app server includes earlier turns in Responses input. The latest marked
    # delegation owns this request, even if the fixture has started another one.
    for item in reversed(body['input']):
        if item.get('role') != 'user':
            continue
        for content in reversed(item['content']):
            match = re.search(r'<input>Fixture delegation ([a-z-]+)\.', content.get('text', ''))
            if match:
                return match[1]
    raise AssertionError('missing fixture delegation in Responses input')
