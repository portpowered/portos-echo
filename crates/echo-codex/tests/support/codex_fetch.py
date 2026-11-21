"""T3 shipped helper CLI with private, offline, already-provisioned cache copies."""
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(sys.argv[1]).resolve()
COMMAND = ['sh', str(ROOT / 'scripts/codex-app-server.sh')]
HELPER = ('codex.release', 'scripts/codex-app-server.sh', 'scripts/codex_release.py')
POINTER = 'current.json'


def run(cache=None, command=COMMAND, release=None):
    # Offline only: selecting a release needs the network, which T3 never uses.
    environment = dict(os.environ, ECHO_CODEX_OFFLINE='1')
    environment.pop('CODEX_RELEASE', None)
    if release is not None:
        environment['CODEX_RELEASE'] = release
    if cache is not None:
        environment['ECHO_CODEX_CACHE_DIR'] = str(cache)
    return subprocess.run(command, env=environment, capture_output=True, text=True, timeout=20)


def successful(result):
    assert result.returncode == 0, result.stderr
    lines = result.stdout.splitlines()
    assert len(lines) == 1, 'helper emitted more than the executable path'
    binary = pathlib.Path(lines[0])
    assert binary.is_absolute() and binary.is_file(), 'unpublished executable path'
    return binary


def refused(result, diagnostic):
    assert result.returncode != 0, 'invalid cache accepted'
    assert result.stdout == '', 'partial executable path published'
    assert diagnostic in result.stderr, result.stderr


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def corrupt_and_restore(path, cache):
    before = digest(path)
    with path.open('r+b') as target:
        original = target.read(1)
        assert original, 'empty cached artifact'
        target.seek(0)
        target.write(bytes([original[0] ^ 1]))
    try:
        refused(run(cache), 'SHA-256 mismatch')
    finally:
        with path.open('r+b') as target:
            target.write(original)
    assert digest(path) == before


def host_cache_integrity(published, cache):
    host = published / 'codex-code-mode-host'
    record = published / 'code-mode-host.sha256'
    corrupt_and_restore(host, cache)
    corrupt_and_restore(record, cache)
    for path in (host, record):
        retained = path.with_name(path.name + '.retained')
        path.rename(retained)
        try:
            refused(run(cache), 'codex-app-server:')
        finally:
            retained.rename(path)
    mode = host.stat().st_mode
    host.chmod(mode & ~0o111)
    try:
        refused(run(cache), 'invalid cached executable')
    finally:
        host.chmod(mode)
    print('host_corruption=refused host_record_corruption=refused host_missing=refused')
    print('host_record_missing=refused host_nonexecutable=refused')


def legacy_cache_requires_provisioning(published, cache):
    legacy = published.with_name(published.name.split('-', 1)[0])
    published.rename(legacy)
    try:
        refused(run(cache), 'binary missing; provision before testing')
        assert legacy.is_dir(), 'legacy cache was modified'
    finally:
        legacy.rename(published)
    assert successful(run(cache)).parent == published
    print('legacy_identity=refused paired_identity=verified')


def provisioned_release(published, cache):
    """The offline reader follows the recorded release and nothing else."""
    pointer = cache / POINTER
    original = pointer.read_text()
    record = json.loads(original)
    assert sorted(record) == ['assets', 'commit', 'tag'], 'unexpected release record'
    assert published == cache / record['commit'] / published.parent.name / '-'.join(record['assets'].values())
    assert successful(run(cache, release=record['tag'])).parent == published
    refused(run(cache, release='rust-v0.0.1'), 'binary missing; provision before testing')
    try:
        for changed in ('', '{', json.dumps({**record, 'commit': 'main'}),
                        json.dumps({**record, 'assets': {name: '0' * 64 for name in record['assets']}}),
                        json.dumps({**record, 'assets': {}})):
            pointer.write_text(changed)
            refused(run(cache), 'binary missing; provision before testing')
        pointer.unlink()
        refused(run(cache), 'binary missing; provision before testing')
    finally:
        pointer.write_text(original)
    assert successful(run(cache)).parent == published
    print('release_record=followed other_release=refused invalid_record=refused missing_record=refused')


def writer_lock(published, cache):
    sys.path.insert(0, str(ROOT / 'crates/echo-codex/tests'))
    from codex_readers import check_warm

    check_warm(published, cache, sys.modules[__name__])
    # No publication exists here: a foreign lease is neither read nor removed.
    cold_cache = cache.parent / 'cold leased cache'
    cold_published = cold_cache / pathlib.Path(*published.parts[-3:])
    cold_published.parent.mkdir(parents=True)
    lock = cold_published.with_name(cold_published.name + '.lock')
    lock.mkdir()
    try:
        refused(run(cold_cache), 'binary missing; provision before testing')
        shutil.copy2(cache / POINTER, cold_cache / POINTER)
        refused(run(cold_cache), 'binary missing; provision before testing')
        assert lock.exists(), 'helper deleted another writer lease'
        assert list(cold_published.parent.iterdir()) == [lock]
    finally:
        lock.rmdir()


def external_worktree(temporary, cache, binary):
    checkout = temporary / 'main checkout'
    checkout.mkdir()
    (checkout / 'scripts').mkdir()
    for name in HELPER:
        shutil.copy2(ROOT / name, checkout / name)

    def git(*arguments):
        return subprocess.check_output(['git', '-C', str(checkout), *arguments],
                                       stderr=subprocess.STDOUT, text=True, timeout=10).strip()

    git('init', '--quiet')
    git('add', *HELPER)
    git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'helper fixture')
    lane = temporary / 'external linked worktree'
    git('worktree', 'add', '--quiet', '--detach', str(lane))
    command = ['sh', str(lane / 'scripts/codex-app-server.sh')]
    assert successful(run(cache, command)) == binary
    assert successful(run(cache, command)) == binary
    refused(run(temporary / 'missing cache', command), 'binary missing; provision before testing')
    for root in (lane, checkout):
        refused(run(root / 'cache', command), 'cache must be outside repository')
    alias = temporary / 'cache alias'
    alias.symlink_to(checkout, target_is_directory=True)
    refused(run(alias / 'cache', command), 'cache must be outside repository')
    # Git permits relative gitdir entries as well as the absolute entry it wrote.
    marker = lane / '.git'
    gitdir = pathlib.Path(marker.read_text().strip().removeprefix('gitdir: '))
    marker.write_text('gitdir: ' + os.path.relpath(gitdir, lane) + '\n')
    assert successful(run(cache, command)) == binary
    refused(run(checkout / 'cache', command), 'cache must be outside repository')
    marker.unlink()
    refused(run(cache, command), 'repository metadata missing')
    print('external_worktree=verified_reuse offline_miss=reported lane_cache=refused shared_cache=refused')
    print('symlink_cache=refused relative_gitdir=verified missing_metadata=refused')


def main():
    source = successful(run())
    with tempfile.TemporaryDirectory(prefix='echo codex cache ') as temporary:
        cache = pathlib.Path(temporary).resolve() / 'private cache with spaces'
        refused(run(cache), 'binary missing; provision before testing')
        relative = pathlib.Path(*source.parent.parts[-3:])
        published = cache / relative
        shutil.copytree(source.parent, published)
        refused(run(cache), 'binary missing; provision before testing')
        shutil.copy2(source.parents[3] / POINTER, cache / POINTER)
        binary = successful(run(cache))
        assert binary == published / source.name
        assert successful(run(cache)) == binary
        archives = list(published.glob('*.tar.gz'))
        assert len(archives) == 2
        for archive in archives:
            corrupt_and_restore(archive, cache)
        corrupt_and_restore(binary, cache)
        host_cache_integrity(published, cache)
        legacy_cache_requires_provisioning(published, cache)
        provisioned_release(published, cache)
        writer_lock(published, cache)
        assert successful(run(cache)) == binary
        external_worktree(pathlib.Path(temporary).resolve(), cache, binary)
    print('offline_missing=refused warm_path=stable archive_corruption=refused binary_corruption=refused')
    print('writer_lease=retained concurrent_paths=one no_partial_publication=true')
    print('FETCH_CACHE_PASS')


if __name__ == '__main__':
    main()
