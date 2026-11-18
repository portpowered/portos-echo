"""T3 shipped helper CLI with private, offline, already-provisioned cache copies."""
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


def run(cache=None, command=COMMAND, offline=True):
    environment = dict(os.environ, ECHO_CODEX_OFFLINE='1' if offline else '0')
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
    if WINDOWS:
        return  # Windows bundles the companion; its helper is unchanged.
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
    if WINDOWS:
        return
    legacy = published.with_name(published.name.split('-', 1)[0])
    published.rename(legacy)
    try:
        refused(run(cache), 'binary missing; provision before testing')
        assert legacy.is_dir(), 'legacy cache was modified'
    finally:
        legacy.rename(published)
    assert successful(run(cache)).parent == published
    print('legacy_identity=refused paired_identity=verified')


def writer_lock(published, cache):
    sys.path.insert(0, str(ROOT / 'crates/echo-codex/tests'))
    from codex_readers import check_warm

    check_warm(published, cache, sys.modules[__name__])
    # No publication exists here: online admission must refuse before download.
    cold_cache = cache.parent / 'cold leased cache'
    cold_published = cold_cache / pathlib.Path(*published.parts[-3:])
    cold_published.parent.mkdir(parents=True)
    lock = cold_published.with_name(cold_published.name + '.lock')
    if WINDOWS:
        lock.write_text('fixture-owned', encoding='ascii')
    else:
        lock.mkdir()
    try:
        refused(run(cold_cache), 'binary missing; provision before testing')
        refused(run(cold_cache, offline=False), 'codex-app-server:')
        assert lock.exists(), 'helper deleted another writer lease'
        if WINDOWS:
            assert lock.read_text(encoding='ascii') == 'fixture-owned'
        assert list(cold_published.parent.iterdir()) == [lock]
    finally:
        lock.unlink() if WINDOWS else lock.rmdir()


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


def windows_host_architecture(cache, binary):
    if not WINDOWS:
        print('wow64=SKIP reason=Windows-only host architecture regression')
        return
    cases = [('F1', 'AMD64', '', True), ('F2', 'x86', 'AMD64', True),
             ('F3', 'x86', '', False), ('F4', 'ARM64', '', False),
             ('F5', 'AMD64', 'ARM64', False)]
    for case, architecture, host, accepted in cases:
        environment = dict(os.environ, ECHO_CODEX_OFFLINE='1',
                           ECHO_CODEX_CACHE_DIR=str(cache),
                           ECHO_HELPER_TEST_PATH=str(ROOT / 'scripts/codex-app-server.ps1'),
                           ECHO_TEST_ARCH=architecture, ECHO_TEST_HOST_ARCH=host)
        # PowerShell normalizes architecture at startup; set it in the child.
        script = ('$env:PROCESSOR_ARCHITECTURE = $env:ECHO_TEST_ARCH; '
                  '$env:PROCESSOR_ARCHITEW6432 = $env:ECHO_TEST_HOST_ARCH; '
                  '& $env:ECHO_HELPER_TEST_PATH; exit $LASTEXITCODE')
        result = subprocess.run(
            ['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', script],
            env=environment, capture_output=True, text=True, timeout=20)
        assert result.returncode == (0 if accepted else 1), (
            f'{case}: exit={result.returncode} stderr={result.stderr.strip()}')
        if accepted:
            assert successful(result) == binary, f'{case}: changed cached path'
            assert result.stderr == '', f'{case}: unexpected stderr'
        else:
            assert result.stdout == '', f'{case}: unsupported host published a path'
            assert result.stderr.strip() == 'codex-app-server: unsupported host', result.stderr
    print('wow64=accepted native_path=same x86=refused arm64=refused host_precedence=verified')


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
        windows_host_architecture(cache, binary)
        archives = list(published.glob('*.zip' if WINDOWS else '*.tar.gz'))
        assert len(archives) == (1 if WINDOWS else 2)
        for archive in archives:
            corrupt_and_restore(archive, cache)
        corrupt_and_restore(binary, cache)
        host_cache_integrity(published, cache)
        legacy_cache_requires_provisioning(published, cache)
        writer_lock(published, cache)
        assert successful(run(cache)) == binary
        external_worktree(pathlib.Path(temporary).resolve(), cache, binary)
    print('offline_missing=refused warm_path=stable archive_corruption=refused binary_corruption=refused')
    print('writer_lease=retained concurrent_paths=one no_partial_publication=true')
    print('FETCH_CACHE_PASS')


if __name__ == '__main__':
    main()
