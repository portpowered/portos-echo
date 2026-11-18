"""Local Responses/delegation observer; emits no arbitrary model input or secrets."""
import json
import struct
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
    def __init__(self, tools, negative, exhausted=False, emotion=False):
        self.emotion = emotion
        self.tools = tools
        self.negative = negative
        self.exhausted = exhausted
        self.reply = "I couldn't call test echo." if negative else 'test echo: delegated echo'
        if emotion and not negative:
            self.reply = 'Here is a happy face.'
        self.models = []
        self.delegations = []
        self.appends = []
        self.failure = None

    def begin(self):
        assert not self.delegations, 'duplicate delegation'
        self.delegations.append('delegation-local')
        return {'type': 'delegation.created', 'item': {
            'type': 'delegation', 'target': 'client', 'id': 'delegation-local',
            'content': [{'type': 'input_text', 'text': 'Call ui_show_emotion with name happy and report the result.' if self.emotion else 'Call test_echo with text delegated echo and report the result.'}]}}

    def respond(self, body):
        expected = [{'type': 'function', 'name': tool['name'], 'description': tool['description'],
                     # rust-v0.160.0 closes each dynamic object schema on the wire.
                     'parameters': {**deepcopy(tool['inputSchema']), 'additionalProperties': False},
                     'strict': False} for tool in self.tools]
        # The same pin drops numeric minimum while lowering JSON schema to its
        # tool-schema type. Assert that measured representation exactly.
        expected[1]['parameters']['properties']['ms'].pop('minimum')
        assert body['tools'] == expected, 'model exposed tools beyond exact dynamic catalog'
        assert body['model'] == 'gpt-5.4', 'wrong delegated model'
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output']
        index = len(self.models)
        assert index < 2, 'unexpected model retry'
        if index == 0:
            assert not outputs, 'premature tool output'
            item = {'type': 'function_call', 'id': 'fc-local', 'call_id': 'tool-local',
                    'name': 'ui_show_emotion' if self.emotion else 'test_echo',
                    'arguments': '{"name":"happy"}' if self.emotion else '{"text":"delegated echo"}', 'status': 'completed'}
        else:
            assert len(outputs) == 1 and outputs[0]['call_id'] == 'tool-local', 'tool identity mismatch'
            # The pinned engine encodes dynamic-tool content as a JSON text value.
            self.check_output(outputs[0]['output'])
            item = {'type': 'message', 'id': 'msg-local', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': self.reply, 'annotations': []}]}
        self.models.append({'tools': [tool['name'] for tool in body['tools']], 'outputs': len(outputs)})
        response = {'id': f'response-{index}', 'status': 'completed', 'output': [item]}
        events = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}},
                  {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                  {'type': 'response.completed', 'response': response}]
        return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events).encode()

    def check_output(self, output):
        if self.negative:
            assert output == 'tool execution failed', 'exact failure output mismatch'
        elif self.emotion:
            content = json.loads(output)
            assert len(content) == 1 and content[0]['type'] == 'text', 'emotion content mismatch'
            assert json.loads(content[0]['text']) == {
                'state': 'emotion', 'name': 'happy', 'frame_ms': 80,
                'frames': ['(^_^)', '(^o^)']}, 'exact animation result mismatch'
        else:
            assert json.loads(output) == [{'type': 'text', 'text': 'delegated echo'}], 'exact tool output mismatch'

    def append(self, value):
        if self.exhausted:
            raise AssertionError('interrupted turn appended a false result')
        assert len(self.models) == 2, 'append before verified output'
        assert value['delegation_item_id'] == 'delegation-local', 'wrong delegation correlation'
        assert value['content'] == [{'type': 'input_text', 'text': self.reply}], 'wrong appended result'
        self.appends.append(value['delegation_item_id'])
        assert len(self.appends) == 1, 'duplicate append'

    def snapshot(self):
        return {'delegations': self.delegations, 'models': self.models, 'appends': self.appends}


class McpDelegation:
    """M5's two native turns, with exact retained model history and outputs."""
    exhausted = False
    reply = 'm5-linked'

    def __init__(self, tools):
        self.tools = tools
        self.models = []
        self.delegations = []
        self.appends = []
        self.failure = None

    def begin(self):
        index = len(self.delegations)
        assert index < 2 and len(self.appends) == index, 'M5 overlapping delegation'
        name = ['m5-linked', 'm5-replayed'][index]
        self.delegations.append(name)
        return {'type': 'delegation.created', 'item': {
            'type': 'delegation', 'target': 'client', 'id': name,
            'content': [{'type': 'input_text', 'text': 'Call test_echo with text m5-linked and report the result.'}]}}

    def respond(self, body):
        expected = [{'type': 'function', 'name': tool['name'], 'description': tool['description'],
                     'parameters': {**deepcopy(tool['inputSchema']), 'additionalProperties': False},
                     'strict': False} for tool in self.tools]
        assert body['tools'] == expected and len(expected) == 1, 'M5 exact dynamic catalog mismatch'
        assert body['model'] == 'gpt-5.4', 'M5 wrong delegated model'
        outputs = [item for item in body['input'] if item.get('type') == 'function_call_output']
        index = len(self.models)
        assert index < 4 and len(self.delegations) == index // 2 + 1, 'M5 model order/retry mismatch'
        expected_outputs = [[{'type': 'text', 'text': 'm5-linked'}],
                            'tool authorization required']
        assert len(outputs) == (index + 1) // 2, 'M5 output count mismatch'
        for position, output in enumerate(outputs):
            assert output['call_id'] == f'm5-call-{position}', 'M5 tool identity mismatch'
            actual = json.loads(output['output']) if position == 0 else output['output']
            assert actual == expected_outputs[position], 'M5 exact native output mismatch'
        if index % 2 == 0:
            item = {'type': 'function_call', 'id': f'm5-fc-{index}', 'call_id': f'm5-call-{index // 2}',
                    'name': 'test_echo', 'arguments': '{"text":"m5-linked"}', 'status': 'completed'}
        else:
            item = {'type': 'message', 'id': f'm5-msg-{index}', 'role': 'assistant', 'status': 'completed',
                    'content': [{'type': 'output_text', 'text': self.reply, 'annotations': []}]}
        self.models.append({'tools': [tool['name'] for tool in body['tools']], 'outputs': len(outputs)})
        response = {'id': f'm5-response-{index}', 'status': 'completed', 'output': [item]}
        events = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}},
                  {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                  {'type': 'response.completed', 'response': response}]
        return ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n' for event in events).encode()

    def append(self, value):
        index = len(self.appends)
        assert len(self.models) == (index + 1) * 2, 'M5 append before verified output'
        assert value['delegation_item_id'] == self.delegations[index], 'M5 append identity mismatch'
        assert value['content'] == [{'type': 'input_text', 'text': self.reply}], 'M5 append content mismatch'
        self.appends.append(value['delegation_item_id'])

    def snapshot(self):
        return {'delegations': self.delegations, 'models': self.models, 'appends': self.appends}
