"""T1 download policy and T3 local transport, shell, and GNU Make witnesses."""

import contextlib
import email.message
import hashlib
import http.client
import http.server
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
import uuid
from unittest import mock

import codex_release

ROOT = Path(__file__).resolve().parents[1]
ASSET_URL = 'https://github.com/openai/codex/releases/download/test/asset'
TAG = 'rust-v999.0.0'
COMMIT = 'a' * 40
CARGO_ARGS = ['nextest', 'run', '--workspace', '--all-targets', '--locked', '--config-file',
              '.github/nextest.toml', '--profile', 'ci', '--partition', 'count:2/4', '--no-tests=pass']


def http_error(status, retry_after=None):
    headers = email.message.Message()
    if retry_after is not None:
        headers['Retry-After'] = retry_after
    return urllib.error.HTTPError(ASSET_URL, status, 'fixture response', headers, io.BytesIO())


class DownloadPolicyTests(unittest.TestCase):
    def fetch_with(self, responses, failure=None):
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(codex_release.urllib.request, 'build_opener') as opener, \
                mock.patch.object(codex_release.time, 'sleep') as sleep, \
                mock.patch.object(codex_release.time, 'time', return_value=784111777):
            request = opener.return_value.open
            request.side_effect = responses
            path = Path(directory) / 'download'
            if failure:
                with self.assertRaisesRegex(ValueError, failure):
                    codex_release.fetch(ASSET_URL, path)
                self.assertFalse(path.exists())
            else:
                codex_release.fetch(ASSET_URL, path)
                self.assertEqual(path.read_bytes(), b'release')
            self.assertEqual(request.call_count, len(responses))
            self.assertEqual([call.kwargs['timeout'] for call in request.call_args_list], [120] * len(responses))
            self.assertEqual(len({id(call.args[0]) for call in request.call_args_list}), len(responses))
            for response in responses:
                if isinstance(response, urllib.error.HTTPError):
                    self.assertTrue(response.fp.closed)
            return [call.args[0] for call in sleep.call_args_list]

    def test_four_fallback_retries_cover_rate_limits_server_and_url_errors(self):
        responses = [http_error(403), http_error(429), http_error(599),
                     urllib.error.URLError('fixture connection'), io.BytesIO(b'release')]
        self.assertEqual(self.fetch_with(responses), [2, 4, 8, 16])

    def test_retry_after_accepts_seconds_dates_and_past_dates(self):
        responses = [http_error(429, '7'), http_error(503, 'Sun, 06 Nov 1994 08:50:00 GMT'),
                     http_error(500, 'Sun, 06 Nov 1994 08:49:00 GMT'), http_error(403, 'invalid'),
                     io.BytesIO(b'release')]
        self.assertEqual(self.fetch_with(responses), [7, 23, 0, 16])

    def test_permanent_http_and_exhausted_url_errors_stop_with_safe_messages(self):
        self.assertEqual(self.fetch_with([http_error(404)], r'^download failed \(HTTP 404\)$'), [])
        responses = [urllib.error.URLError(uuid.uuid4().hex) for _ in range(5)]
        self.assertEqual(self.fetch_with(responses, r'^download failed \(network error\)$'), [2, 4, 8, 16])

    def test_exact_https_host_and_token_precedence(self):
        primary, fallback = uuid.uuid4().hex, uuid.uuid4().hex
        cases = [
            ('https://GITHUB.com/a', primary, fallback, primary),
            ('https://raw.githubusercontent.com/a', '', fallback, fallback),
            ('https://github.com/a', '', '', None),
            ('https://github.com.example/a', primary, fallback, None),
            ('https://api.github.com/a', primary, fallback, None),
            ('https://github.com@example.test/a', primary, fallback, None),
            ('http://github.com/a', primary, fallback, None),
        ]
        for url, gh, github, expected in cases:
            with self.subTest(url=url), mock.patch.dict(os.environ, GH_TOKEN=gh, GITHUB_TOKEN=github):
                request = codex_release.authenticate_download(urllib.request.Request(url))
                header = 'Bearer ' + expected if expected else None
                self.assertTrue(request.get_header('Authorization') == header, 'unexpected bearer scope or precedence')


def loopback_open(port):
    def open_request(handler, request):
        # Keep the original URL, Host and redirect behavior. Replace only the socket destination.
        def connect(_host, **options):
            return http.client.HTTPConnection('127.0.0.1', port, **options)

        return handler.do_open(connect, request)

    return open_request


@contextlib.contextmanager
def loopback_transport(port):
    with mock.patch.object(urllib.request, 'getproxies', dict), \
            mock.patch.object(urllib.request.HTTPHandler, 'http_open', loopback_open(port)), \
            mock.patch.object(urllib.request.HTTPSHandler, 'https_open', loopback_open(port)):
        yield


class ReleasePeer(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        host = self.headers['Host']
        authorization = self.headers.get('Authorization')
        credential = self.server.bearers.get(authorization, 'unexpected')
        self.server.requests.append((host, self.path, credential))
        responses = self.server.routes.get((host, self.path), [(404, b'')])
        status, body, *options = responses.pop(0) if len(responses) > 1 else responses[0]
        headers, reason = options or ({}, None)
        self.send_response(status, reason)
        for key, value in headers.items():
            self.send_header(key, value)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        pass


class LoopbackCase(unittest.TestCase):
    def setUp(self):
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.primary, self.fallback = uuid.uuid4().hex, uuid.uuid4().hex
        self.server = self.enterContext(http.server.HTTPServer(('127.0.0.1', 0), ReleasePeer))
        self.server.bearers = {None: 'absent', 'Bearer ' + self.primary: 'primary',
                               'Bearer ' + self.fallback: 'fallback'}
        self.server.routes = {}
        self.server.requests = []
        thread = threading.Thread(target=self.server.serve_forever)
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(self.server.shutdown)
        self.port = self.server.server_port

    def assert_redacted(self, *output):
        self.assertFalse(any(marker in line for line in output for marker in (self.primary, self.fallback)),
                         'credential appeared in captured output')


class LoopbackDownloadTests(LoopbackCase):
    def setUp(self):
        super().setUp()
        self.enterContext(loopback_transport(self.port))
        self.sleep = self.enterContext(mock.patch.object(codex_release.time, 'sleep'))
        self.stderr = self.enterContext(contextlib.redirect_stderr(io.StringIO()))

    def download(self, url, primary='', fallback=''):
        with mock.patch.dict(os.environ, GH_TOKEN=primary, GITHUB_TOKEN=fallback):
            target = self.temp / 'download'
            codex_release.fetch(url, target)
            self.assertEqual(target.read_bytes(), b'download bytes')
        self.assert_redacted(self.stderr.getvalue())
        self.assertEqual(self.stderr.getvalue(), '')

    def test_403_then_200_succeeds_on_attempt_two(self):
        endpoint = ('github.com', '/asset')
        self.server.routes[endpoint] = [(403, b''), (200, b'download bytes')]
        self.download('https://github.com/asset', self.primary)
        self.assertEqual(self.server.requests, [(*endpoint, 'primary')] * 2)
        self.sleep.assert_called_once_with(2)
        print('download retry: HTTP 403 -> 200, attempts=2, bearer=present, stderr=empty', flush=True)

    def test_loopback_sees_bearer_only_for_eligible_host_and_environment(self):
        cases = [
            ('raw.githubusercontent.com', self.fallback, 'fallback'),
            ('github.com', '', 'absent'),
            ('github.com.example', self.fallback, 'absent'),
        ]
        for host, token, expected in cases:
            self.server.routes[host, '/notice'] = [(200, b'download bytes')]
            self.download(f'https://{host}/notice', fallback=token)
            self.assertEqual(self.server.requests[-1], (host, '/notice', expected))
        print('download bearer: raw fallback=present, no token=absent, lookalike=absent', flush=True)

    def test_redirect_rechecks_https_hostname_on_each_hop(self):
        hops = [
            ('github.com', '/asset', 'https://raw.githubusercontent.com/next', 'primary'),
            ('raw.githubusercontent.com', '/next', 'https://release-assets.githubusercontent.com/asset?signed=1',
             'primary'),
            ('release-assets.githubusercontent.com', '/asset?signed=1', 'https://github.com/continue', 'absent'),
            ('github.com', '/continue', 'http://github.com/downgrade', 'primary'),
        ]
        for host, path, location, _ in hops:
            self.server.routes[host, path] = [(302, b'', {'Location': location}, None)]
        final = ('github.com', '/downgrade')
        self.server.routes[final] = [(200, b'download bytes')]
        self.download('https://github.com/asset', self.primary)
        self.assertEqual(self.server.requests, [(host, path, auth) for host, path, _, auth in hops]
                         + [(*final, 'absent')])
        print('download redirects: allowed HTTPS=present, downgrade=absent, CDN=absent', flush=True)


def release_archive(member):
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode='w:gz') as bundle:
        content = b'#!/bin/sh\nexit 0\n'
        entry = tarfile.TarInfo(member)
        entry.size = len(content)
        bundle.addfile(entry, io.BytesIO(content))
    return archive.getvalue()


class MakeProvisionTests(LoopbackCase):
    def setUp(self):
        super().setUp()
        self.cache = self.temp / 'cache'
        self.cargo_calls = self.temp / 'cargo-args'
        self.cargo_calls.touch()
        bin_dir = self.temp / 'bin'
        bin_dir.mkdir()
        self.write_executable(bin_dir / 'cargo', 'printf "%s\\n" "$@" >> "$WITNESS_CARGO_CALLS"\n')
        self.write_executable(bin_dir / 'python3', 'exec "$WITNESS_PYTHON" -B "$WITNESS_LAUNCHER" "$@"\n')
        launcher = self.temp / 'launch.py'
        launcher.write_text(
            'import runpy, sys, time\n'
            'from test_codex_release import loopback_transport\n'
            'sys.argv.pop(0)\n'
            'time.sleep = lambda _: None\n'
            f'with loopback_transport({self.port}):\n'
            '    runpy.run_path(sys.argv[0], run_name="__main__")\n'
        )
        self.env = dict(os.environ, GH_TOKEN=self.primary, GITHUB_TOKEN='', CODEX_RELEASE=TAG,
                        ECHO_CODEX_CACHE_DIR=str(self.cache), ECHO_CODEX_OFFLINE='0',
                        PYTHONPATH=str(ROOT / 'scripts'), WITNESS_CARGO_CALLS=str(self.cargo_calls),
                        WITNESS_PYTHON=sys.executable, WITNESS_LAUNCHER=str(launcher),
                        PATH=str(bin_dir) + os.pathsep + os.environ['PATH'])
        for key in ('MAKEFLAGS', 'GNUMAKEFLAGS', 'MAKEOVERRIDES', 'MFLAGS'):
            self.env.pop(key, None)
        self.assets, self.cold_routes = self.seed_release()

    def write_executable(self, path, shell):
        path.write_text('#!/bin/sh\n' + shell)
        path.chmod(0o700)

    def seed_release(self):
        assets = []
        downloads = {}
        for prefix in ('codex-', 'codex-code-mode-host-'):
            member = prefix + 'x86_64-unknown-linux-musl'
            archive = release_archive(member)
            name = member + '.tar.gz'
            assets.append({'name': name, 'digest': 'sha256:' + hashlib.sha256(archive).hexdigest()})
            downloads['github.com', f'/openai/codex/releases/download/{TAG}/{name}'] = [(200, archive)]
        api = 'api.github.com'
        self.server.routes[api, f'/repos/openai/codex/releases/tags/{TAG}'] = [
            (200, json.dumps({'tag_name': TAG, 'assets': assets}).encode())]
        self.server.routes[api, f'/repos/openai/codex/commits/{TAG}'] = [(200, COMMIT.encode())]
        for name in ('LICENSE', 'NOTICE'):
            downloads['raw.githubusercontent.com', f'/openai/codex/{COMMIT}/{name}'] = [(200, name.encode())]
        self.server.routes.update(downloads)
        return assets, list(self.server.routes)

    def invoke(self, command, **overrides):
        result = subprocess.run(command, cwd=ROOT, env=dict(self.env, **overrides),
                                capture_output=True, text=True, timeout=20)
        self.assert_redacted(result.stdout, result.stderr)
        return result

    def provision(self):
        result = self.invoke(['sh', 'scripts/codex-app-server.sh'])
        self.assertEqual(result.returncode, 0, result.stderr)
        binary = Path(result.stdout.strip())
        expected_digests = '-'.join(asset['digest'].split(':')[1] for asset in self.assets)
        self.assertEqual(binary, self.cache / COMMIT / 'x86_64-unknown-linux-musl' / expected_digests
                         / 'codex-x86_64-unknown-linux-musl')
        self.assertEqual(binary.read_bytes(), b'#!/bin/sh\nexit 0\n')
        self.assertEqual(self.server.requests, [(*route, 'primary') for route in self.cold_routes])
        return binary

    def make(self, tag=TAG):
        return self.invoke(['make', 'test-unit-rust', 'NEXTEST_PARTITION=count:2/4'],
                           ECHO_CODEX_OFFLINE='1', CODEX_RELEASE=tag)

    def test_terminal_download_failure_redacts_stderr_and_publishes_nothing(self):
        name = self.assets[0]['name']
        asset = ('github.com', f'/openai/codex/releases/download/{TAG}/{name}')
        self.server.routes[asset] = [(403, b'', {}, 'rate limit exceeded ' + self.primary)]
        result = self.invoke(['sh', 'scripts/codex-app-server.sh'])
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, '')
        self.assertEqual(result.stderr, 'codex-app-server: download failed (HTTP 403: rate limit exceeded)\n')
        self.assertEqual(self.server.requests[2:], [(*asset, 'primary')] * 5)
        self.assertEqual(list(self.cache.glob('*/' + codex_release.TARGET + '/*')), [])
        self.assertFalse((self.cache / 'current.json').exists())
        print('failed provision: attempts=5, exit=1, stderr=sanitized, published artifacts=0', flush=True)

    def test_make_reuses_verified_pair_with_zero_requests_and_one_cargo_call(self):
        binary = self.provision()
        before = self.server.requests.copy()
        result = self.make()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([line for line in result.stdout.splitlines() if line.startswith(str(self.cache))],
                         [str(binary)])
        self.assertEqual(self.cargo_calls.read_text().splitlines(), CARGO_ARGS)
        self.assertEqual(self.server.requests, before)
        print(f'Make cache hit: cold HTTP requests={len(before)}, warm HTTP requests=0, same path, Cargo calls=1',
              flush=True)

    def test_make_rejects_missing_mismatched_and_corrupt_cache_without_network(self):
        binary = self.provision()
        before = self.server.requests.copy()
        cases = [
            ('archive', binary.parent / self.assets[1]['name'], TAG, 'SHA-256 mismatch'),
            ('binary', binary.parent / 'codex-code-mode-host', TAG, 'SHA-256 mismatch'),
            ('missing', self.cache / 'current.json', TAG, 'binary missing; provision before testing'),
            ('mismatch', None, 'rust-v999.0.1', 'binary missing; provision before testing'),
        ]
        for case, path, tag, message in cases:
            with self.subTest(case=case):
                original = path.read_bytes() if path is not None else None
                try:
                    if case == 'missing':
                        path.unlink()
                    elif path is not None:
                        path.write_bytes(b'corrupt')
                    result = self.make(tag)
                    self.assertEqual(result.returncode, 2)
                    self.assertNotIn(str(self.cache), result.stdout)
                    self.assertEqual(result.stderr.splitlines()[0], 'codex-app-server: ' + message)
                    self.assertEqual(self.cargo_calls.read_text(), '')
                    self.assertEqual(self.server.requests, before)
                finally:
                    if path is not None:
                        path.write_bytes(original)
        print('Make cache failures: archive, binary, missing, mismatch; HTTP requests=0, Cargo calls=0', flush=True)
