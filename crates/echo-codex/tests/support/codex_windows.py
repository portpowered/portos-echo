"""Strict Windows T3 fetch runner and network-free T1 completion checks."""
import os
import pathlib
import subprocess
import sys
import unittest


WOW64 = ('wow64=accepted native_path=same x86=refused arm64=refused '
         'host_precedence=verified')
COMPLETE = 'FETCH_CACHE_PASS'


def validate_preconditions(platform, version, optimize, environment):
    host = (environment.get('PROCESSOR_ARCHITEW6432') or
            environment.get('PROCESSOR_ARCHITECTURE'))
    if platform != 'win32' or host != 'AMD64':
        raise ValueError('requires Windows AMD64 host')
    if version < (3, 11) or optimize != 0:
        raise ValueError('requires Python 3.11+ and enabled assertions')
    if environment.get('ECHO_CODEX_OFFLINE') != '1':
        raise ValueError('requires ECHO_CODEX_OFFLINE=1')
    if not environment.get('ECHO_CODEX_CACHE_DIR'):
        raise ValueError('requires preprovisioned external ECHO_CODEX_CACHE_DIR')


def validate_completion(returncode, stdout, stderr):
    if returncode != 0:
        raise ValueError(f'fetch fixture failed: exit={returncode}\n{stdout}{stderr}')
    if 'wow64=SKIP' in stdout + stderr:
        raise ValueError('Windows architecture assertions skipped')
    lines = stdout.splitlines()
    if lines.count(WOW64) != 1:
        raise ValueError('requires exactly one complete WOW64 result')
    if lines.count(COMPLETE) != 1:
        raise ValueError('requires exactly one FETCH_CACHE_PASS result')
    return stdout


def main(arguments):
    validate_preconditions(sys.platform, sys.version_info[:2], sys.flags.optimize, os.environ)
    if len(arguments) != 1 or not pathlib.Path(arguments[0]).is_absolute():
        raise ValueError('requires one absolute checkout root')
    root = pathlib.Path(arguments[0]).resolve(strict=True)
    cache = pathlib.Path(os.environ['ECHO_CODEX_CACHE_DIR'])
    if not cache.is_absolute():
        raise ValueError('cache must be absolute')
    cache = cache.resolve(strict=True)
    # Include the main checkout when invoked from an in-repository worktree.
    for checkout in (root, *root.parents):
        if (checkout / '.git').exists() and cache.is_relative_to(checkout):
            raise ValueError('cache must be outside repository')
    if not cache.is_dir():
        raise ValueError('cache must be a preprovisioned directory')
    result = subprocess.run(
        [sys.executable, str(root / 'crates/echo-codex/tests/support/codex_fetch.py'), str(root)],
        capture_output=True, text=True, timeout=240)
    output = validate_completion(result.returncode, result.stdout, result.stderr)
    sys.stdout.write(output)
    sys.stderr.write(result.stderr)


class CompletionTests(unittest.TestCase):
    """T1: explicit result records; no helper, process, cache or network."""

    def test_accepts_and_preserves_actual_output(self):
        output = 'cache detail\n' + WOW64 + '\n' + COMPLETE + '\n'
        self.assertEqual(validate_completion(0, output, ''), output)

    def test_rejects_failed_child_even_with_success_markers(self):
        with self.assertRaisesRegex(ValueError, 'exit=1'):
            validate_completion(1, WOW64 + '\n' + COMPLETE, 'child failure')

    def test_rejects_skip_in_either_stream(self):
        output = WOW64 + '\n' + COMPLETE + '\n'
        for stdout, stderr in ((output + 'wow64=SKIP', ''), (output, 'wow64=SKIP')):
            with self.subTest(stderr=stderr), self.assertRaisesRegex(ValueError, 'skipped'):
                validate_completion(0, stdout, stderr)

    def test_rejects_missing_malformed_or_duplicate_architecture(self):
        for line in ('', WOW64.replace('same', 'different'), 'prefix ' + WOW64,
                     WOW64 + ' suffix', WOW64 + '\n' + WOW64):
            with self.subTest(line=line), self.assertRaisesRegex(ValueError, 'WOW64'):
                validate_completion(0, line + '\n' + COMPLETE, '')

    def test_rejects_missing_malformed_or_duplicate_completion(self):
        for line in ('', COMPLETE + ' suffix', COMPLETE + '\n' + COMPLETE):
            with self.subTest(line=line), self.assertRaisesRegex(ValueError, 'FETCH_CACHE_PASS'):
                validate_completion(0, WOW64 + '\n' + line, '')

    def test_stderr_markers_cannot_supply_completion(self):
        with self.assertRaisesRegex(ValueError, 'WOW64'):
            validate_completion(0, '', WOW64 + '\n' + COMPLETE)

    def test_valid_preconditions_including_wow64_host(self):
        for architecture, override in (('AMD64', ''), ('x86', 'AMD64')):
            environment = dict(PROCESSOR_ARCHITECTURE=architecture,
                               PROCESSOR_ARCHITEW6432=override,
                               ECHO_CODEX_OFFLINE='1', ECHO_CODEX_CACHE_DIR='external')
            self.assertIsNone(validate_preconditions('win32', (3, 11), 0, environment))

    def test_rejects_each_invalid_precondition(self):
        valid = dict(PROCESSOR_ARCHITECTURE='AMD64', ECHO_CODEX_OFFLINE='1',
                     ECHO_CODEX_CACHE_DIR='external')
        cases = [('linux', (3, 11), 0, valid), ('win32', (3, 10), 0, valid),
                 ('win32', (3, 11), 1, valid)]
        for changes in ({'PROCESSOR_ARCHITECTURE': 'x86'},
                        {'PROCESSOR_ARCHITECTURE': 'ARM64'},
                        {'PROCESSOR_ARCHITEW6432': 'ARM64'},
                        {'ECHO_CODEX_OFFLINE': ''}, {'ECHO_CODEX_OFFLINE': '0'},
                        {'ECHO_CODEX_CACHE_DIR': ''}):
            cases.append(('win32', (3, 11), 0, dict(valid, **changes)))
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                validate_preconditions(*case)


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except (ValueError, OSError, subprocess.TimeoutExpired) as error:
        sys.exit(f'codex-windows: {error}')
