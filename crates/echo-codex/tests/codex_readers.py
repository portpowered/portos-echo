"""T3 real offline helper readers; provision the release archives before testing."""
import concurrent.futures
import pathlib
import shutil
import sys
import tempfile
import threading


def concurrent_readers(published, cache, fixture):
    barrier = threading.Barrier(8)

    def read(_):
        barrier.wait(timeout=20)
        return fixture.run(cache)

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(read, range(8)))
    paths = [fixture.successful(result) for result in results]
    assert len(paths) == 8 and len(set(paths)) == 1
    assert paths[0].parent == published
    assert all(result.stderr == '' for result in results)


def check_warm(published, cache, fixture):
    concurrent_readers(published, cache, fixture)
    lock = published.with_name(published.name + '.lock')
    lock.mkdir()
    try:
        concurrent_readers(published, cache, fixture)
        binary = published / 'codex-x86_64-unknown-linux-musl'
        mode = binary.stat().st_mode
        binary.chmod(mode & ~0o111)
        try:
            fixture.refused(fixture.run(cache), 'invalid cached executable')
        finally:
            binary.chmod(mode)
        for name in ('LICENSE', 'NOTICE'):
            notice = published / name
            retained = published / (name + '.retained')
            notice.rename(retained)
            try:
                fixture.refused(fixture.run(cache), 'cached attribution missing')
            finally:
                retained.rename(notice)
        assert lock.exists(), 'helper deleted another writer lease'
    finally:
        lock.rmdir()
    assert sorted(path.name for path in published.parent.iterdir()) == [published.name]
    print('readers=8 identical_paths=true foreign_lease=retained')
    print('CONCURRENT_READERS_PASS')


def main():
    from support import codex_fetch as fixture

    source = fixture.successful(fixture.run())
    with tempfile.TemporaryDirectory(prefix='echo codex readers ') as temporary:
        cache = pathlib.Path(temporary).resolve() / 'private cache with spaces'
        published = cache / pathlib.Path(*source.parent.parts[-3:])
        shutil.copytree(source.parent, published)
        shutil.copy2(source.parents[3] / fixture.POINTER, cache / fixture.POINTER)
        check_warm(published, cache, fixture)


if __name__ == '__main__':
    main()
