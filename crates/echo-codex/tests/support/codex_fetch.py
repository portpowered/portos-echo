"""T3 shipped helper CLI with private, offline, already-provisioned cache copies."""
import concurrent.futures
import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(sys.argv[1]).resolve()
WINDOWS = sys.platform == 'win32'
COMMAND = (['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
            str(ROOT / 'scripts/codex-app-server.ps1')] if WINDOWS else
           ['sh', str(ROOT / 'scripts/codex-app-server.sh')])


def run(cache=None, command=COMMAND):
    environment = dict(os.environ, ECHO_CODEX_OFFLINE='1')
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


def writer_lock(published, cache):
    lock = published.with_name(published.name + '.lock')
    if WINDOWS:
        lock.write_text('fixture-owned', encoding='ascii')
    else:
        lock.mkdir()
    try:
        # Both CLI processes encounter an independently held writer lease.
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(run, [cache, cache]))
        for result in results:
            refused(result, 'codex-app-server:')
            assert lock.exists(), 'helper deleted another writer lease'
        if WINDOWS:
            assert lock.read_text(encoding='ascii') == 'fixture-owned'
    finally:
        lock.unlink() if WINDOWS else lock.rmdir()
    # Once admitted, competing readers may succeed or refuse an owned lease.
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(run, [cache, cache]))
    successes = [successful(result) for result in results if result.returncode == 0]
    assert successes and all(path.parent == published for path in successes)
    for result in results:
        if result.returncode != 0:
            refused(result, 'codex-app-server:')
    assert sorted(path.name for path in published.parent.iterdir()) == [published.name]


def external_worktree(temporary, cache, binary):
    if WINDOWS:
        return  # This regression is in the Linux helper.
    checkout = temporary / 'main checkout'
    checkout.mkdir()
    (checkout / 'scripts').mkdir()
    for name in ('codex.pin', 'scripts/codex-app-server.sh'):
        shutil.copy2(ROOT / name, checkout / name)

    def git(*arguments):
        return subprocess.check_output(['git', '-C', str(checkout), *arguments],
                                       stderr=subprocess.STDOUT, text=True, timeout=10).strip()

    git('init', '--quiet')
    git('add', 'codex.pin', 'scripts/codex-app-server.sh')
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
        binary = successful(run(cache))
        assert binary == published / source.name
        assert successful(run(cache)) == binary
        archives = list(published.glob('*.zip' if WINDOWS else '*.tar.gz'))
        assert len(archives) == 1
        corrupt_and_restore(archives[0], cache)
        corrupt_and_restore(binary, cache)
        writer_lock(published, cache)
        assert successful(run(cache)) == binary
        external_worktree(pathlib.Path(temporary).resolve(), cache, binary)
    print('offline_missing=refused warm_path=stable archive_corruption=refused binary_corruption=refused')
    print('writer_lease=retained concurrent_paths=one no_partial_publication=true')
    print('FETCH_CACHE_PASS')


if __name__ == '__main__':
    main()
