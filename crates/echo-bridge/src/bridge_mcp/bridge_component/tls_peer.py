import base64, http.server, json, ssl, sys, threading, urllib.parse
counts = dict(link=0, token=0, catalog=0, query=0)
key = None
release = threading.Event()
requested_scopes = sys.argv[1]
tools = ("control", "endpoint_show", "endpoint_query") if sys.argv[2] == "full" else ("endpoint_query",)
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def respond(self, status, value, challenge=None):
        data = b'' if value is None else json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        if challenge is not None: self.send_header('WWW-Authenticate', challenge)
        self.end_headers()
        try: self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError): pass
    def do_GET(self):
        if self.path == '/.well-known/oauth-protected-resource':
            self.respond(200, dict(resource=base+'/mcp', authorization_servers=[base]))
        else:
            self.respond(200, dict(issuer=base, device_authorization_endpoint=base+'/link',
                token_endpoint=base+'/token', revocation_endpoint=base+'/revoke',
                dpop_signing_alg_values_supported=['ES256']))
    def proof(self):
        global key
        parts = self.headers['DPoP'].split('.')
        decode = lambda p: json.loads(base64.urlsafe_b64decode(p + '=' * (-len(p) % 4)))
        header, claims = decode(parts[0]), decode(parts[1])
        assert header['alg'] == 'ES256' and header['typ'] == 'dpop+jwt'
        assert claims['htm'] == 'POST' and claims['htu'] == base+self.path
        assert 'ath' not in claims
        if key is not None: assert header['jwk'] == key
        key = header['jwk']
    def do_POST(self):
        data = self.rfile.read(int(self.headers['Content-Length']))
        if self.path in ('/link', '/token'):
            self.proof()
            form = urllib.parse.parse_qs(data.decode())
            if self.path == '/link':
                assert form['scope'] == [requested_scopes]
                assert form['resource'] == [base+'/mcp']
                counts['link'] += 1
                self.respond(200, dict(device_code='synthetic-device', user_code='TEST-CODE',
                    verification_uri=base+'/approve', expires_in=1800, interval=1))
            else:
                counts['token'] += 1
                self.respond(200, dict(access_token='synthetic-access', refresh_token='synthetic-refresh',
                    token_type='Bearer', expires_in=3600, scope='endpoint:read' if counts['link'] == 1 else requested_scopes))
            return
        assert self.path == '/mcp'
        assert self.headers.get('DPoP') is None
        assert self.headers['Authorization'] == 'Bearer synthetic-access'
        request = json.loads(data)
        method = request['method']
        if method == 'notifications/initialized':
            self.respond(202, None)
            return
        if method == 'initialize':
            result = dict(protocolVersion='2025-03-26', capabilities=dict(tools={}),
                          serverInfo=dict(name='local', version='1'))
        elif method == 'tools/list':
            counts['catalog'] += 1
            result = dict(tools=[dict(name=name, description='Synthetic tool',
                          inputSchema=dict(type='object', properties={})) for name in tools])
        else:
            if request['params']['name'] == 'control':
                self.control(request)
                return
            assert method == 'tools/call' and request['params']['name'] in tools
            kind = 'query' if request['params']['name'] == 'endpoint_query' else 'show'
            counts[kind] = counts.get(kind, 0) + 1
            mode = request['params']['arguments'].get('mode')
            if mode == 'pending':
                print('pending', flush=True)
                release.wait()
            if mode == 'http':
                self.respond(503, {})
                return
            if mode == 'malformed':
                self.respond(200, {})
                return
            result = dict(content=[dict(type='text', text='synthetic-query-result')], isError=mode == 'error')
        self.respond(200, dict(jsonrpc='2.0', id=request['id'], result=result))
    def control(self, request):
        assert request['method'] == 'tools/call'
        counts['control'] = counts.get('control', 0) + 1
        mode = request['params']['arguments'].get('mode')
        if mode == 'unauthorized':
            self.respond(401, {})
        elif mode == 'forbidden':
            self.respond(403, {})
        elif mode == 'unknown':
            self.respond(403, {}, 'Bearer error="insufficient_scope", scope="unrecognized:scope"')
        elif counts['link'] == 1:
            self.respond(403, {}, 'Bearer error="insufficient_scope", scope="message:send"')
        else:
            counts['effects'] = counts.get('effects', 0) + 1
            self.respond(200, dict(jsonrpc='2.0', id=request['id'],
                result=dict(content=[dict(type='text', text='accepted')], isError=False)))
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain('ca.pem', 'key.pem')
server.socket = context.wrap_socket(server.socket, server_side=True)
base = 'https://127.0.0.1:' + str(server.server_address[1])
worker = threading.Thread(target=server.serve_forever)
worker.start()
print(server.server_address[1], flush=True)
for command in sys.stdin:
    if command.strip() != 'release': break
    release.set()
release.set()
server.shutdown()
worker.join()
server.server_close()
print(json.dumps(counts), flush=True)
