"""Component T3 launcher and offline replay commands for the catalog spike."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

from catalog import CASES, DIRECT_VARIANTS, declarations, load, render


class Rpc:
    def __init__(self, child):
        self.child = child
        self.messages = queue.Queue()
        self.sequence = 0
        self.pending = []
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        for line in self.child.stdout:
            self.messages.put(json.loads(line))
        self.messages.put(None)

    def send(self, value):
        self.child.stdin.write(json.dumps(value) + '\n')
        self.child.stdin.flush()

    def receive(self, deadline):
        try:
            message = self.messages.get(timeout=max(0, deadline - time.monotonic()))
        except queue.Empty as error:
            raise AssertionError('pin JSON-RPC deadline') from error
        assert message is not None, 'pin closed JSON-RPC'
        return message

    def request(self, method, params, expected_error=None):
        self.sequence += 1
        self.send({'id': self.sequence, 'method': method, 'params': params})
        deadline = time.monotonic() + 4
        while True:
            message = self.receive(deadline)
            if message.get('id') == self.sequence and 'method' not in message:
                if expected_error:
                    error = message.get('error', {})
                    assert error.get('code') == -32600 and expected_error in error.get('message', ''), (
                        f'{method}: missing expected validation of {expected_error}')
                    return None
                assert 'error' not in message, f'{method}: pin error {message.get("error", {}).get("code")}'
                return message['result']
            self.pending.append(message)

    def turn(self, peers, turn, thread):
        deadline = time.monotonic() + 4
        while True:
            message = self.pending.pop(0) if self.pending else self.receive(deadline)
            if message.get('method') == 'item/tool/call':
                self.send(turn.dispatch(peers.origin, message, thread))
            if message.get('method') == 'turn/completed' and message['params']['threadId'] == thread:
                return message['params']['turn']['status']


def environment(home, origin):
    env = {'CODEX_HOME': str(home), 'HOME': str(home), 'PATH': str(home),
           'NO_PROXY': '127.0.0.1', 'no_proxy': '127.0.0.1'}
    for key in ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy'):
        env[key] = origin
    return env


@contextmanager
def pin(binary, config, home, origin):
    command = [str(binary), 'app-server', '--listen', 'stdio://']
    for key in config:
        command.extend(['-c', key])
    with subprocess.Popen(command, env=environment(home, origin), cwd=home, text=True,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as child:
        rpc = Rpc(child)
        try:
            yield rpc
        finally:
            try:
                child.stdin.close()
                assert child.wait(timeout=3) == 0, 'pin exited unsuccessfully'
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                raise AssertionError('pin failed to exit after input closed') from None
            finally:
                rpc.reader.join(timeout=3)
                assert not rpc.reader.is_alive(), 'pin output reader was not reaped'


def app_settings(origin):
    provider = {'name': 'local', 'base_url': f'{origin}/v1', 'wire_api': 'responses',
                'requires_openai_auth': True, 'experimental_bearer_token': 'dummy-local-only',
                'request_max_retries': 0, 'stream_max_retries': 0}
    config = ['model_provider="local"',
              'model_providers.local={' + ','.join(f'{k}={json.dumps(v)}' for k, v in provider.items()) + '}',
              f'experimental_realtime_webrtc_call_base_url="{origin}/backend-api/codex"',
              f'experimental_realtime_ws_base_url="{origin}/v1"',
              'features.skip_host_skill_discovery=true']
    for key in ('features.plugins', 'features.recommended_plugins', 'features.remote_models', 'features.apps',
                'features.remote_control', 'features.skill_search', 'cloud.skills.enabled',
                'analytics.enabled', 'feedback.enabled', 'check_for_update_on_startup'):
        config.append(f'{key}=false')
    return config


def thread_settings(tools, variant, model, home):
    dynamic, config = declarations(tools, variant)
    for feature in ('shell_tool', 'unified_exec', 'multi_agent', 'multi_agent_v2', 'goals'):
        config[f'features.{feature}'] = False
    config.update({'web_search': 'disabled', 'mcp_servers': {}, 'agents.enabled': False,
                   'tools.experimental_request_user_input.enabled': False, 'tools.update_plan.enabled': False})
    return {'model': model, 'cwd': str(home), 'dynamicTools': dynamic, 'ephemeral': True,
            'approvalPolicy': 'never', 'sandbox': 'read-only', 'environments': [], 'config': config,
            'baseInstructions': 'Use the declared tools when appropriate.'}


def reserved_functions(rpc, home, tool):
    from peer import MODEL

    params = thread_settings([tool], 'flat', MODEL, home)
    params['dynamicTools'] = [{**tool, 'deferLoading': True}]
    rpc.request('thread/start', params, 'deferred dynamic tool must include a namespace')
    params['dynamicTools'] = [{'type': 'namespace', 'name': 'functions', 'description': 'Port OS tools.',
                               'tools': [{'type': 'function', **tool, 'deferLoading': True}]}]
    rpc.request('thread/start', params, 'reserved Responses API namespace: functions')
    print('F12: pin rejects deferral in the stock functions namespace')


def measure(rpc, peers, home, tools, case, variant, fault=None):
    from peer import MODEL, OFFER, Turn, session

    turn = Turn(case, variant, [tool['name'] for tool in tools], fault)
    session.script = turn
    thread = rpc.request('thread/start', thread_settings(tools, variant, MODEL, home))['thread']['id']
    rpc.request('thread/realtime/start', {'threadId': thread, 'outputModality': 'audio',
                'transport': {'type': 'webrtc', 'sdp': OFFER}, 'version': 'v3',
                'includeStartupContext': False, 'clientManagedHandoffs': False})
    peers.delegate()
    try:
        status = rpc.turn(peers, turn, thread)
        assert not peers.failures, f'{turn.id}: {peers.failures}'
        assert status == ('failed' if fault == 'peer-close' else 'completed'), f'{turn.id}: unexpected turn {status}'
        if status == 'completed':
            assert turn.appended.wait(3), f'{turn.id}: missing delegation handback'
    finally:
        rpc.request('thread/realtime/stop', {'threadId': thread})
        assert turn.sideband_closed.wait(3), f'{turn.id}: sideband did not close'
    assert session.observed['external'] == 0, 'app server attempted non-fixture traffic'
    if fault:
        expected = {'missing': f'missing reachable candidate {case.expected}',
                    'peer-close': 'model peer closed while request was pending',
                    'unsupported': 'client tool_search hidden by code-mode-only'}
        assert turn.selected is None and turn.calls == [] and turn.diagnostic == expected[fault]
        assert turn.appended.is_set() == (fault != 'peer-close') and len(turn.raw) == 1
        print(f'{turn.id}: {turn.diagnostic}, MCP calls=0, result=diagnostic')
        return None, turn.raw
    selected = turn.selected[1] if turn.selected else None
    assert selected == case.expected and turn.calls == [case.expected], f'{turn.id}: wrong selection or call'
    assert len(turn.raw) == 2, f'{turn.id}: wrong round trip count'
    print(f'{variant} {case.id} {selected} MCP calls=1 round_trips={len(turn.raw)}')
    return {'variant': variant, 'case': case.id, 'tool': selected, 'round_trips': len(turn.raw)}, turn.raw


def schema(binary, home, origin):
    directory = home / 'schema'
    subprocess.run([str(binary), 'app-server', 'generate-json-schema', '--experimental', '--out', str(directory)],
                   env=environment(home, origin), cwd=home, check=True, timeout=5, stdout=subprocess.DEVNULL)
    text = (directory / 'v2/ThreadStartParams.json').read_text(encoding='utf-8')
    markers = {'dynamicTools': '"dynamicTools":', 'deferLoading': '"deferLoading":',
               'namespace variant': '"title": "NamespaceDynamicToolSpec"'}
    for field, marker in markers.items():
        lines = [str(n) for n, line in enumerate(text.splitlines(), 1) if line.lstrip().startswith(marker)]
        assert lines, f'generated experimental schema is missing {field}'
        print(f'v2/ThreadStartParams.json:{",".join(lines)} {field}')


def replay(binary):
    tools, capture = load(os.environ)
    from peer import Peers

    print('catalog mode=' + ('full' if capture else 'smoke'))
    with tempfile.TemporaryDirectory(prefix='echo-tool-catalog-') as temp, Peers() as peers:
        home = Path(temp)
        standalone = home / 'codex'
        shutil.copy2(binary, standalone)
        schema(standalone, home, peers.origin)
        with pin(standalone, app_settings(peers.origin), home, peers.origin) as rpc:
            rpc.request('initialize', {'clientInfo': {'name': 'echo-lab', 'version': '0.1.0'},
                                      'capabilities': {'experimentalApi': True}})
            rpc.send({'method': 'initialized'})
            rows = []
            for variant in DIRECT_VARIANTS:
                for case in CASES if capture else CASES[:1]:
                    row, raw = measure(rpc, peers, home, tools, case, variant)
                    rows.append(row)
                    if capture:
                        for index, request in enumerate(raw):
                            (capture / f'{variant}-{case.id}-{index}.json').write_bytes(request)
            missing = [tool for tool in tools if tool['name'] != CASES[0].expected]
            for variant in DIRECT_VARIANTS:
                row, _ = measure(rpc, peers, home, missing, CASES[0], variant, 'missing')
                assert row is None
            row, _ = measure(rpc, peers, home, tools, CASES[0], 'deferred', 'unsupported')
            assert row is None
            reserved_functions(rpc, home, tools[0])
            measure(rpc, peers, home, tools, CASES[0], 'forced-direct')
            row, _ = measure(rpc, peers, home, tools, CASES[0], 'flat', 'peer-close')
            assert row is None
    if capture:
        (capture / 'selections.json').write_text(json.dumps(rows), encoding='utf-8')


if __name__ == '__main__':
    match sys.argv[1:]:
        case ['replay', binary]:
            replay(binary)
        case ['render', capture]:
            render(Path(capture))
        case ['unit']:
            import unittest

            unittest.main(module='test_catalog', argv=[sys.argv[0]])
        case _:
            raise SystemExit('expected unit, replay <pin>, or render <capture>')
