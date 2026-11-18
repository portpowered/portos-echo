#!/bin/sh
# Python 3.12 is a repository prerequisite. The pin is data, never shell code.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
[ "$#" -eq 0 ] || {
  printf '%s\n' 'codex-app-server: takes no arguments' >&2
  exit 1
}
exec python3 - "$root" <<'PY'
import hashlib
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def fetch(url, path):
    with urllib.request.urlopen(url, timeout=120) as source, path.open('wb') as target:
        shutil.copyfileobj(source, target)


def metadata_path(base, value):
    # Windows-created worktrees can also be invoked through WSL.
    if pathlib.PureWindowsPath(value).is_absolute():
        value = subprocess.check_output(['wslpath', '-u', value], text=True).strip()
    return (base / value).resolve(strict=True)


def checkout_roots(root):
    marker = root / '.git'
    if marker.is_dir():
        return (root,)
    if not marker.is_file():
        raise ValueError('repository metadata missing')
    prefix, separator, value = marker.read_text().strip().partition(': ')
    if prefix != 'gitdir' or not separator:
        raise ValueError('invalid repository metadata')
    gitdir = metadata_path(root, value)
    common = gitdir / 'commondir'
    if common.is_file():
        shared = metadata_path(gitdir, common.read_text().strip())
        return (root, shared.parent)
    return (root,)


def verified_binary(published, assets, target):
    for asset, name, record in assets:
        binary = published / name
        if digest(published / asset['name']) != asset['sha256'] or digest(binary) != (published / record).read_text().strip():
            raise ValueError('SHA-256 mismatch')
        if not os.access(binary, os.X_OK):
            raise ValueError('invalid cached executable')
    if not all((published / name).is_file() for name in ('LICENSE', 'NOTICE')):
        raise ValueError('cached attribution missing')
    return published / ('codex-' + target)


def stage_asset(stage, pin, asset, member_name, binary_name, record):
    archive = stage / asset['name']
    fetch(pin['repo'] + '/releases/download/' + pin['tag'] + '/' + asset['name'], archive)
    if digest(archive) != asset['sha256']:
        raise ValueError('SHA-256 mismatch')
    with tarfile.open(archive) as bundle:
        members = bundle.getmembers()
        if len(members) != 1 or members[0].name != member_name or not members[0].isfile():
            raise ValueError('unexpected archive member')
        bundle.extractall(stage, filter='data')
    binary = stage / binary_name
    if member_name != binary_name:
        (stage / member_name).rename(binary)
    binary.chmod(0o700)
    (stage / record).write_text(digest(binary) + '\n')


def main():
    root = pathlib.Path(sys.argv[1]).resolve()
    pin = tomllib.loads((root / 'codex.pin').read_text())
    commit = 'a956835d020762cb2b570053af06f643a11c0ecc'
    if (pin['repo'], pin['tag'], pin['commit']) != ('https://github.com/openai/codex', 'rust-v0.160.0', commit):
        raise ValueError('invalid pin')
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'AMD64'):
        raise ValueError('unsupported host; Windows uses codex-app-server.ps1')
    target = 'x86_64-unknown-linux-musl'
    asset = pin['assets'][target]
    expected = '306865417d4ee7a927785852910a527f41e1e159add390ac5ae3accb67d44a13'
    if asset != {'name': 'codex-x86_64-unknown-linux-musl.tar.gz', 'sha256': expected}:
        raise ValueError('invalid asset pin')
    host = pin['assets']['code-mode-host-' + target]
    host_expected = 'ac6cd6288f0e39f46a33eba1cfd37711651f14f33dbc73d6a1b82eb45e038cac'
    if host != {'name': 'codex-code-mode-host-' + target + '.tar.gz', 'sha256': host_expected}:
        raise ValueError('invalid code-mode host pin')
    assets = ((asset, 'codex-' + target, 'binary.sha256'),
              (host, 'codex-code-mode-host', 'code-mode-host.sha256'))
    default = pathlib.Path(os.environ.get('XDG_CACHE_HOME', pathlib.Path.home() / '.cache')) / 'portos-echo/codex'
    cache = pathlib.Path(os.environ.get('ECHO_CODEX_CACHE_DIR', default))
    if not cache.is_absolute():
        raise ValueError('cache must be absolute')
    cache = cache.resolve()
    # Reject caches inside any checkout, including this lane's shared main checkout.
    if any(cache.is_relative_to(checkout) for checkout in checkout_roots(root)):
        raise ValueError('cache must be outside repository')
    parent = cache / commit / target
    parent.mkdir(parents=True, exist_ok=True)
    # The paired identity leaves legacy codex-only caches untouched and forces a
    # fresh atomic publication with both verified executables on first upgrade.
    published = parent / (expected + '-' + host_expected)
    lock = published.with_name(published.name + '.lock')
    if published.exists():
        print(verified_binary(published, assets, target))
        return
    if os.environ.get('ECHO_CODEX_OFFLINE') == '1':
        raise ValueError('binary missing; provision before testing')
    try:
        lock.mkdir()
    except FileExistsError as error:
        raise ValueError('another fetch owns this pin; lock retained') from error
    try:
        if published.exists():
            binary = verified_binary(published, assets, target)
        else:
            with tempfile.TemporaryDirectory(prefix='fetch-', dir=parent) as stage:
                stage = pathlib.Path(stage)
                stage_asset(stage, pin, asset, 'codex-' + target, 'codex-' + target, 'binary.sha256')
                stage_asset(stage, pin, host, 'codex-code-mode-host-' + target,
                            'codex-code-mode-host', 'code-mode-host.sha256')
                for notice in ('LICENSE', 'NOTICE'):
                    fetch('https://raw.githubusercontent.com/openai/codex/' + commit + '/' + notice, stage / notice)
                # Rename only after extraction, digest and attribution have succeeded.
                os.rename(stage, published)
            binary = published / ('codex-' + target)
        print(binary)
    finally:
        lock.rmdir()


try:
    main()
except Exception as error:
    print('codex-app-server: ' + str(error), file=sys.stderr)
    sys.exit(1)
PY
