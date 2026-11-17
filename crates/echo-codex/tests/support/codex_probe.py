"""T3 S5 regression probe: real pin, synthetic call/sideband/Responses only."""
import base64
import hashlib
import http.server
import json
import os
import pathlib
import queue
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import urllib.parse
# Linux is authoritative for confinement; Windows proves routing/proxy only.
if sys.platform == 'linux' and '--confined' not in sys.argv:
    raise SystemExit(subprocess.call(['unshare', '--user', '--map-root-user', '--net', sys.executable, str(pathlib.Path(__file__).resolve()), *sys.argv[1:], '--confined']))
if sys.platform == 'linux':
    subprocess.run(['ip', 'link', 'set', 'lo', 'up'], check=True)
    links = json.loads(subprocess.check_output(['ip', '-json', 'link']))
    assert [link['ifname'] for link in links] == ['lo'], links
    assert subprocess.check_output(['ip', 'route']).strip() == b''
    try:
        socket.create_connection(('192.0.2.1', 9), timeout=1)
    except OSError:
        print('network_namespace=loopback_only external_negative_control=refused', flush=True)
    else:
        raise AssertionError('non-loopback connection escaped namespace')
observed = {'calls': [], 'sideband': [], 'responses': [], 'append': [], 'external': []}
case = sys.argv[sys.argv.index('--case') + 1] if '--case' in sys.argv else 'happy'
appended = threading.Event()
connections = set()
connections_lock = threading.Lock()
connections_empty = threading.Event()
connections_empty.set()
ANSWER = 'v=0\r\ns=fake-answer\r\n'

def frame(sock, value):
    data = json.dumps(value).encode()
    head = bytes([129, len(data)]) if len(data) < 126 else bytes([129, 126]) + struct.pack('!H', len(data))
    sock.sendall(head + data)

def exact(sock, n):
    out = b''
    while len(out) < n:
        part = sock.read(n - len(out))
        if not part:
            raise EOFError()
        out += part
    return out

def read_frame(sock):
    a, b = exact(sock, 2)
    n = b & 127
    if n == 126:
        n = struct.unpack('!H', exact(sock, 2))[0]
    elif n == 127:
        n = struct.unpack('!Q', exact(sock, 8))[0]
    assert n <= 1048576, (a, b, n)
    mask = exact(sock, 4) if b & 128 else b''
    data = exact(sock, n)
    if mask:
        data = bytes((x ^ mask[i % 4] for i, x in enumerate(data)))
    return (a & 15, data)

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def setup(self):
        super().setup()
        with connections_lock:
            connections.add(self.connection)
            connections_empty.clear()

    def finish(self):
        try:
            super().finish()
        finally:
            with connections_lock:
                connections.remove(self.connection)
                if not connections:
                    connections_empty.set()

    def log_message(self, *a):
        pass

    def respond(self, status, data, kind='application/json', headers={}):
        self.send_response(status)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(data)))
        for k, v in headers.items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()

    def do_CONNECT(self):
        observed['external'].append(self.path)
        self.respond(403, b'refused')

    def do_GET(self):
        if self.path != '/v1/live/rtc_local':
            self.respond(404, b'{}')
            return
        observed['sideband'].append(self.path)
        if case == 'sideband-401':
            self.respond(401, b'{}')
            return
        key = base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', key)
        self.end_headers()
        self.connection.settimeout(20)
        frame(self.connection, {'type': 'session.started', 'session': {'id': 'session-local'}})
        for role, text in [('user', 'ping'), ('assistant', 'echo-ok')]:
            kind = 'input' if role == 'user' else 'output'
            frame(self.connection, {'type': kind + '_transcript.added', 'item': {'text': text}})
            frame(self.connection, {'type': 'turn.done', 'turn': {'role': role, 'transcript': text}})
        frame(self.connection, {'type': 'delegation.created', 'item': {'type': 'delegation', 'target': 'client', 'id': 'delegation-local', 'content': [{'type': 'input_text', 'text': 'Call echo_test with message ping and report echo-ok.'}]}})
        try:
            while True:
                op, data = read_frame(self.rfile)
                if op == 8:
                    return
                if op != 1:
                    continue
                value = json.loads(data)
                if value.get('type') == 'delegation.context.append':
                    observed['append'].append(value)
                    appended.set()
        except (EOFError, OSError):
            pass
        self.close_connection = True

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        if self.path.startswith('/backend-api/codex/realtime/calls'):
            observed['calls'].append({'path': self.path, 'body': body})
            if case.startswith('call-'):
                self.respond(int(case.removeprefix('call-')), b'{"error":{"message":"synthetic refusal"}}')
                return
            self.respond(201, ANSWER.encode(), 'application/sdp', {'Location': '/v1/live/rtc_local'})
            return
        if self.path != '/v1/responses':
            self.respond(404, b'{}')
            return
        observed['responses'].append(body)
        if case.startswith('responses-'):
            self.respond(int(case.removeprefix('responses-')), b'{"error":{"message":"synthetic model refusal"}}')
            return
        count = len(observed['responses'])
        if count == 1:
            item = {'type': 'function_call', 'id': 'fc-local', 'call_id': 'tool-local', 'name': 'echo_test', 'arguments': '{"message":"ping"}', 'status': 'completed'}
        else:
            item = {'type': 'message', 'id': 'msg-local', 'role': 'assistant', 'status': 'completed', 'content': [{'type': 'output_text', 'text': 'echo-ok', 'annotations': []}]}
        response = {'id': f'response-{count}', 'status': 'completed', 'output': [item]}
        events = [{'type': 'response.created', 'response': {'id': response['id'], 'status': 'in_progress', 'output': []}}, {'type': 'response.output_item.done', 'output_index': 0, 'item': item}, {'type': 'response.completed', 'response': response}]
        data = ''.join(('event: ' + x['type'] + '\ndata: ' + json.dumps(x) + '\n\n' for x in events))
        self.respond(200, data.encode(), 'text/event-stream')

def verify(sdps, tools, started, closed):
    normalized = []
    for body in observed['responses']:
        normalized.append([x.get('name', x.get('type')) for x in body['tools']])
    print(json.dumps({'calls': [{'path': x['path'], 'model': x['body']['session']['model']} for x in observed['calls']], 'sideband': observed['sideband'], 'model_tools': normalized, 'tool_calls': tools, 'sdp': sdps, 'started': started, 'append': observed['append'], 'closed': closed, 'external_requests': observed['external']}, indent=2), flush=True)
    assert len(observed['calls']) == 1
    call = observed['calls'][0]
    assert call['body']['sdp'] == 'v=0\r\ns=fake-offer\r\n'
    assert urllib.parse.parse_qs(urllib.parse.urlsplit(call['path']).query) == {'intent': ['quicksilver'], 'architecture': ['avas']}, call['path']
    assert call['body']['session']['model'] == 'gpt-live-1-codex', call['body']['session']['model']
    assert sdps == [ANSWER] and started == ['v3'], (sdps, started)
    assert normalized == [['echo_test'], ['echo_test']], normalized
    assert len(tools) == 1 and tools[0]['tool'] == 'echo_test' and (tools[0]['arguments'] == {'message': 'ping'}), tools
    assert len(observed['append']) == 1 and observed['append'][0]['delegation_item_id'] == 'delegation-local', observed['append']
    assert observed['append'][0]['content'] == [{'type': 'input_text', 'text': 'echo-ok'}]
    outputs = [item for item in observed['responses'][1]['input'] if item.get('type') == 'function_call_output']
    assert len(outputs) == 1 and outputs[0]['call_id'] == 'tool-local', outputs
    assert 'echo-ok' in outputs[0]['output'], outputs
    assert not observed['external'], observed['external']
    print('S5_PASS', flush=True)

# Stop can close HTTP keep-alive connections while handlers are reading.
class QuietServer(http.server.ThreadingHTTPServer):

    def handle_error(self, request, client_address):
        pass
server = QuietServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
port = server.server_port
base = f'http://127.0.0.1:{port}'
with tempfile.TemporaryDirectory(prefix='echo-codex-s5-', ignore_cleanup_errors=True) as home:
    overrides = {'model_provider': 'local', 'model': 'gpt-5.4', 'model_providers.local': {'name': 'local', 'base_url': base + '/v1', 'wire_api': 'responses', 'requires_openai_auth': True, 'experimental_bearer_token': 'dummy-local-only'}, 'experimental_realtime_webrtc_call_base_url': base + '/backend-api/codex', 'experimental_realtime_ws_base_url': base + '/v1', 'features.plugins': False, 'features.recommended_plugins': False, 'features.remote_models': False, 'features.apps': False, 'features.remote_control': False, 'features.skill_search': False, 'features.skip_host_skill_discovery': True, 'cloud.skills.enabled': False, 'analytics.enabled': False, 'feedback.enabled': False, 'features.multi_agent': False, 'features.multi_agent_v2': False, 'features.goals': False, 'tools.experimental_request_user_input.enabled': False, 'features.shell_tool': False, 'features.unified_exec': False, 'web_search': 'disabled', 'tools.update_plan.enabled': False, 'mcp_servers': {}, 'cli_auth_credentials_store': 'file', 'check_for_update_on_startup': False}

    def toml(v):
        if isinstance(v, dict):
            return '{' + ','.join((k + '=' + toml(x) for k, x in v.items())) + '}'
        return json.dumps(v)
    if '--rust-client' in sys.argv:
        executable = sys.argv[sys.argv.index('--rust-client') + 1]
        env = dict(os.environ, ECHO_CODEX_JOURNEY=json.dumps({
            'binary': sys.argv[1], 'home': str(pathlib.Path(home) / 'client'),
            'base': base, 'case': case, 'overrides': [(k, toml(v)) for k, v in overrides.items()]}))
        child = subprocess.Popen([executable, '--exact', 't3_pinned_codex_local_call_and_delegation', '--nocapture'],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, env=env)
        report = None
        watcher = None
        try:
            output = queue.Queue()
            def read_rust():
                for line in child.stdout:
                    output.put(line)
                output.put(None)
            threading.Thread(target=read_rust, daemon=True).start()
            while True:
                line = output.get(timeout=25)
                if line is None:
                    break
                if line.startswith('RUST_TOOL_HELD='):
                    if case == 'drop':
                        watcher = subprocess.Popen([sys.executable, str(pathlib.Path(__file__).with_name('codex_watch.py')),
                                                    line.strip().split('=')[1]], stdout=subprocess.PIPE, text=True)
                        assert watcher.stdout.readline().strip() == 'WATCHING'
                    print('ISOLATION_HELD', flush=True)
                    signal = sys.stdin.readline().strip()
                    assert signal == ('drop' if case == 'drop' else 'resume'), signal
                    child.stdin.write(signal + '\n')
                    child.stdin.flush()
                elif line.strip() == 'RUST_TURN_COMPLETE':
                    if case != 'interrupt':
                        assert appended.wait(10), 'append missing after Rust tool response'
                    child.stdin.write('append-observed\n')
                    child.stdin.flush()
                elif line.startswith('RUST_REPORT='):
                    report = json.loads(line.removeprefix('RUST_REPORT='))
            result = child.wait(timeout=10)
            calls = [{'path': x['path'], 'model': x['body']['session']['model']} for x in observed['calls']]
            assert result == 0, ('expected explicit v3, model=gpt-live-1-codex, '
                                 'query=intent=quicksilver&architecture=avas; observed calls=' +
                                 json.dumps(calls) + '; Rust: ' + child.stderr.read())
            assert report is not None, 'Rust journey produced no report'
            if case in ('happy', 'survivor'):
                verify(report['sdp'], report['tools'], report['started'], report['closed'])
                expected = []
                for role, text in [('user', 'ping'), ('assistant', 'echo-ok')]:
                    expected += [dict(method='thread/realtime/transcript/delta', role=role, delta=text, text=None),
                                 dict(method='thread/realtime/transcript/done', role=role, delta=None, text=text)]
                assert report['transcripts'] == expected, report['transcripts']
                print('TRANSCRIPT_PASS user=ping assistant=echo-ok delta_and_done=preserved', flush=True)
            elif case == 'drop':
                assert report == {'dropped': True}, report
                assert len(observed['calls']) == 1 and len(observed['sideband']) == 1
                assert len(observed['responses']) == 1 and observed['append'] == []
                assert [tool['name'] for tool in observed['responses'][0]['tools']] == ['echo_test']
                assert not observed['external']
                assert watcher.stdout.readline().strip() == 'EXITED'
                assert watcher.wait(timeout=5) == 0
            elif case == 'interrupt':
                print(json.dumps({'case': case, 'report': report, 'append': observed['append']}), flush=True)
                assert len(observed['responses']) == 1
                assert [tool['name'] for tool in observed['responses'][0]['tools']] == ['echo_test']
                assert len(report['tools']) == 1 and report['tools'][0]['turnId'] == report['turn_id']
                assert report['closed'] is True and report['sdp'] == [ANSWER] and report['started'] == ['v3']
                assert observed['append'] == []
                assert not observed['external']
            elif case.startswith('responses-'):
                print(json.dumps({'case': case, 'report': report, 'responses': len(observed['responses']),
                                  'append': observed['append']}), flush=True)
                assert len(observed['responses']) == 6 and observed['append'] == []
                assert len(report['errors']) == 6
                for index, error in enumerate(report['errors'][:5], 1):
                    assert error['message'] == f'Reconnecting... {index}/5'
                    assert error['codexErrorInfo'] == {'responseStreamDisconnected': {'httpStatusCode': 401}}
                assert report['errors'][-1]['codexErrorInfo'] == {'httpConnectionFailed': {'httpStatusCode': 401}}
                assert 'synthetic model refusal' in report['errors'][-1]['message']
                assert report['status'] == 'failed' and report['closed'] == 'requested'
                assert report['tools'] == [] and report['errors']
                assert not observed['external']
            else:
                print(json.dumps({'case': case, 'report': report, 'calls': len(observed['calls']),
                                  'sideband': observed['sideband'], 'external': observed['external']}), flush=True)
                assert not observed['responses'] and not observed['append']
                assert not observed['external'] and report['tools'] == []
                if case.startswith('call-'):
                    assert report['sdp'] == []
                    assert not observed['sideband']
                    count = 1 if case == 'call-401' else 5
                    assert len(observed['calls']) == count
                    status = case.removeprefix('call-')
                    assert len(report['errors']) == 1 and f'status {status}' in report['errors'][0]
                    assert 'synthetic refusal' in report['errors'][0]
                    assert report['closed'] == 'requested'
                else:
                    assert len(observed['calls']) == 1 and report['sdp'] == [ANSWER]
                    assert observed['sideband'] == ['/v1/live/rtc_local'] * 5
                    assert report['errors'] == ['unexpected status 401 Unauthorized: realtime websocket handshake failed']
                    assert report['closed'] == 'error'
            if case != 'drop':
                assert report['reaped'] is True
            assert connections_empty.wait(5), 'child left a fake socket open after shutdown'
            print('PUBLIC_CLIENT_PASS', flush=True)
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)
            if watcher is not None:
                if watcher.poll() is None:
                    watcher.kill()
                watcher.wait(timeout=5)
            server.shutdown()
            server.server_close()
        raise SystemExit(0)
    argv = [sys.argv[1], 'app-server', '--listen', 'stdio://']
    for k, v in overrides.items():
        argv += ['-c', k + '=' + toml(v)]
    env = {k: v for k, v in os.environ.items() if k.upper() in ('SYSTEMROOT', 'WINDIR', 'PATH', 'TEMP', 'TMP', 'COMSPEC')}
    env.update(CODEX_HOME=home, HTTP_PROXY=base, HTTPS_PROXY=base, ALL_PROXY=base, NO_PROXY='127.0.0.1', http_proxy=base, https_proxy=base, all_proxy=base, no_proxy='127.0.0.1')
    stderr = open(pathlib.Path(home) / 'stderr.log', 'w')
    child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, env=env, cwd=home)
    messages = queue.Queue()

    def reader():
        for line in child.stdout:
            messages.put(json.loads(line))
        messages.put({'eof': True})
    threading.Thread(target=reader, daemon=True).start()

    def send(value):
        child.stdin.write(json.dumps(value) + '\n')
        child.stdin.flush()

    def request(i, method, params):
        send({'id': i, 'method': method, 'params': params})
        while True:
            value = messages.get(timeout=20)
            if value.get('id') == i:
                assert 'error' not in value, value
                return value['result']
    try:
        init = request(1, 'initialize', {'clientInfo': {'name': 'portos_echo', 'version': '0.1.0'}, 'capabilities': {'experimentalApi': True}})
        print('initialize=' + json.dumps({'userAgent': init['userAgent'], 'platformOs': init['platformOs']}), flush=True)
        send({'method': 'initialized'})
        print('account_before=' + json.dumps(request(2, 'account/read', {'refreshToken': False})), flush=True)
        print('login=' + json.dumps(request(3, 'account/login/start', {'type': 'apiKey', 'apiKey': 'dummy-local-only'})), flush=True)
        thread = request(4, 'thread/start', {'model': 'gpt-5.4', 'modelProvider': 'local', 'cwd': home, 'ephemeral': True, 'approvalPolicy': 'never', 'sandbox': 'read-only', 'environments': [], 'dynamicTools': [{'name': 'echo_test', 'description': 'Synthetic echo', 'inputSchema': {'type': 'object', 'properties': {'message': {'type': 'string'}}, 'required': ['message'], 'additionalProperties': False}}]})
        tid = thread['thread']['id']
        send({'id': 5, 'method': 'thread/realtime/start', 'params': {'threadId': tid, 'outputModality': 'audio', 'transport': {'type': 'webrtc', 'sdp': 'v=0\r\ns=fake-offer\r\n'}, 'version': 'v3', 'prompt': 'Synthetic local fixture.', 'includeStartupContext': False, 'clientManagedHandoffs': False}})
        sdps = []
        tools = []
        started = []
        while not appended.is_set():
            value = messages.get(timeout=20)
            method = value.get('method')
            if method == 'thread/realtime/sdp':
                sdps.append(value['params']['sdp'])
            if method == 'thread/realtime/started':
                started.append(value['params']['version'])
            if method == 'item/tool/call':
                tools.append(value['params'])
                send({'id': value['id'], 'result': {'contentItems': [{'type': 'inputText', 'text': 'echo-ok'}], 'success': True}})
            if method == 'thread/realtime/error' or 'error' in value:
                raise AssertionError(value)
            if len(observed['append']):
                break
            # The event is a readiness barrier, not a fixed delay.
            if method == 'turn/completed':
                assert appended.wait(10), 'append missing'
        send({'id': 6, 'method': 'thread/realtime/stop', 'params': {'threadId': tid}})
        closed = False
        while not closed:
            value = messages.get(timeout=20)
            closed = value.get('method') == 'thread/realtime/closed'
        verify(sdps, tools, started, closed)
    finally:
        child.terminate()
        child.wait(timeout=10)
        stderr.close()
        server.shutdown()
