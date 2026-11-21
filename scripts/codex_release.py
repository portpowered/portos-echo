"""Select, verify and cache the Codex release that Echo builds and tests against.

No release is pinned. The newest stable `rust-v*` release of openai/codex is
selected when this runs online, and `CODEX_RELEASE` names another one to
reproduce an old build. `codex.release` holds the floor. Each official asset is
checked against the SHA-256 digest GitHub publishes for it.

    codex_release.py resolve <root>   print tag=, version= and commit= lines
    codex_release.py host <root>      provision the Linux x86_64 app server, print its path
"""
import datetime
import email.utils
import hashlib
import http.client
import json
import os
import pathlib
import platform
import re
import shutil
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

REPOSITORY = 'https://github.com/openai/codex'
API = 'https://api.github.com/repos/openai/codex'
STABLE = re.compile(r'rust-v(\d+)\.(\d+)\.(\d+)')
TARGET = 'x86_64-unknown-linux-musl'
POINTER = 'current.json'
DOWNLOAD_HOSTS = {'github.com', 'raw.githubusercontent.com'}
DOWNLOAD_DELAYS = (2, 4, 8, 16)


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def authenticate_download(request):
    url = urllib.parse.urlsplit(request.full_url)
    if url.scheme == 'https' and url.hostname in DOWNLOAD_HOSTS:
        token = os.environ.get('GH_TOKEN') or os.environ.get('GITHUB_TOKEN')
        if token:
            # urllib leaves this header behind. The redirect handler checks the next URL.
            request.add_unredirected_header('Authorization', 'Bearer ' + token)
    return request


class DownloadRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        redirected = super().redirect_request(request, response, code, message, headers, url)
        return authenticate_download(redirected) if redirected is not None else None


def retry_delay(value, fallback):
    if not value:
        return fallback
    value = value.strip()
    try:
        if re.fullmatch(r'[0-9]+', value):
            return int(value)
        date = email.utils.parsedate_to_datetime(value)
        if date.tzinfo is None:
            date = date.replace(tzinfo=datetime.timezone.utc)
        return max(0, date.timestamp() - time.time())
    except (ValueError, OverflowError):
        return fallback


def copy_download(source, target):
    while True:
        try:
            chunk = source.read(shutil.COPY_BUFSIZE)
        except (ConnectionError, TimeoutError, http.client.IncompleteRead) as error:
            raise urllib.error.URLError('download interrupted') from error
        if not chunk:
            return
        target.write(chunk)


def open_download(opener, url):
    try:
        return opener.open(authenticate_download(urllib.request.Request(url)), timeout=120)
    except (ConnectionError, TimeoutError) as error:
        raise urllib.error.URLError('download interrupted') from error
    except (ValueError, http.client.HTTPException):
        raise ValueError('download failed (invalid request or response)') from None


def download_failure(error):
    # Preserve the rate-limit signal used by CI monitoring without printing upstream text.
    limited = error.code == 429 or (error.code == 403 and (
        'rate limit exceeded' in str(error.reason).lower() or error.headers.get('X-RateLimit-Remaining') == '0'))
    detail = ': rate limit exceeded' if limited else ''
    return ValueError(f'download failed (HTTP {error.code}{detail})')


def fetch(url, path):
    opener = urllib.request.build_opener(DownloadRedirect())
    for delay in (*DOWNLOAD_DELAYS, None):
        try:
            with open_download(opener, url) as source, path.open('wb') as target:
                copy_download(source, target)
            return
        except urllib.error.HTTPError as error:
            with error:
                if delay is None or not (error.code in (403, 429) or 500 <= error.code <= 599):
                    raise download_failure(error) from None
                delay = retry_delay(error.headers.get('Retry-After'), delay)
        except urllib.error.URLError:
            if delay is None:
                raise ValueError('download failed (network error)') from None
        time.sleep(delay)


def api(path, accept='application/vnd.github+json'):
    headers = {'Accept': accept, 'User-Agent': 'codex-release-resolver', 'X-GitHub-Api-Version': '2022-11-28'}
    # A token only raises the rate limit; the release metadata is public.
    token = os.environ.get('GH_TOKEN') or os.environ.get('GITHUB_TOKEN')
    if token:
        headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request(API + path, headers=headers)
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def version(tag):
    match = STABLE.fullmatch(tag)
    return tuple(int(part) for part in match.groups()) if match else None


def stable(record):
    return not record.get('draft') and not record.get('prerelease') and version(record.get('tag_name', ''))


def newest_stable():
    # Releases are listed newest first, and pre-releases outnumber stable ones.
    for page in range(1, 6):
        records = json.loads(api(f'/releases?per_page=30&page={page}'))
        candidates = [record for record in records if stable(record)]
        if candidates:
            return max(candidates, key=lambda record: version(record['tag_name']))
        if not records:
            break
    raise ValueError('no stable rust-v release found')


def minimum(root):
    settings = tomllib.loads((root / 'codex.release').read_text())
    floor = version('rust-v' + settings.get('minimum', ''))
    if settings.get('repo') != REPOSITORY or floor is None:
        raise ValueError('invalid codex.release')
    return floor


def resolve(root):
    """The release to use: its tag, version, commit and published asset digests."""
    floor = minimum(root)
    requested = os.environ.get('CODEX_RELEASE', '').strip()
    if requested:
        if version(requested) is None:
            raise ValueError('CODEX_RELEASE must name a stable rust-v release tag')
        record = json.loads(api('/releases/tags/' + requested))
    else:
        record = newest_stable()
    tag = record.get('tag_name', '')
    if not stable(record) or (requested and tag != requested):
        raise ValueError('not a published stable release')
    if version(tag) < floor:
        raise ValueError(tag + ' is older than the minimum in codex.release')
    commit = api('/commits/' + tag, 'application/vnd.github.sha').decode().strip()
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('release tag did not resolve to a commit')
    assets = {}
    for asset in record.get('assets', []):
        algorithm, _, value = (asset.get('digest') or '').partition(':')
        if algorithm == 'sha256' and re.fullmatch(r'[0-9a-f]{64}', value):
            assets[asset['name']] = value
    return {'tag': tag, 'version': '.'.join(map(str, version(tag))), 'commit': commit, 'assets': assets}


def metadata_path(base, value):
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


def cache_directory(root):
    default = pathlib.Path(os.environ.get('XDG_CACHE_HOME', pathlib.Path.home() / '.cache')) / 'portos-echo/codex'
    cache = pathlib.Path(os.environ.get('ECHO_CODEX_CACHE_DIR', default))
    if not cache.is_absolute():
        raise ValueError('cache must be absolute')
    cache = cache.resolve()
    # Reject caches inside any checkout, including this lane's shared main checkout.
    if any(cache.is_relative_to(checkout) for checkout in checkout_roots(root)):
        raise ValueError('cache must be outside repository')
    return cache


def host_assets(published_digests):
    """(archive name, expected digest, archive member, executable name, digest record)."""
    rows = (('codex-' + TARGET, 'codex-' + TARGET, 'binary.sha256'),
            ('codex-code-mode-host-' + TARGET, 'codex-code-mode-host', 'code-mode-host.sha256'))
    assets = []
    for member, binary, record in rows:
        expected = published_digests.get(member + '.tar.gz')
        if not isinstance(expected, str) or not re.fullmatch(r'[0-9a-f]{64}', expected):
            raise ValueError('release publishes no SHA-256 digest for ' + member + '.tar.gz')
        assets.append((member + '.tar.gz', expected, member, binary, record))
    return assets


def published_directory(cache, commit, assets):
    # The paired identity leaves codex-only caches untouched and forces a fresh
    # atomic publication with both verified executables.
    return cache / commit / TARGET / '-'.join(expected for _, expected, _, _, _ in assets)


def verified_binary(published, assets):
    for archive, expected, _, binary, record in assets:
        path = published / binary
        if digest(published / archive) != expected or digest(path) != (published / record).read_text().strip():
            raise ValueError('SHA-256 mismatch')
        if not os.access(path, os.X_OK):
            raise ValueError('invalid cached executable')
    if not all((published / name).is_file() for name in ('LICENSE', 'NOTICE')):
        raise ValueError('cached attribution missing')
    return published / assets[0][3]


def stage_asset(stage, tag, asset):
    archive_name, expected, member_name, binary_name, record = asset
    archive = stage / archive_name
    fetch(REPOSITORY + '/releases/download/' + tag + '/' + archive_name, archive)
    if digest(archive) != expected:
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


def publish(published, release, assets):
    lock = published.with_name(published.name + '.lock')
    published.parent.mkdir(parents=True, exist_ok=True)
    try:
        lock.mkdir()
    except FileExistsError as error:
        raise ValueError('another fetch owns this release; lock retained') from error
    try:
        if not published.exists():
            with tempfile.TemporaryDirectory(prefix='fetch-', dir=published.parent) as stage:
                stage = pathlib.Path(stage)
                for asset in assets:
                    stage_asset(stage, release['tag'], asset)
                for notice in ('LICENSE', 'NOTICE'):
                    fetch('https://raw.githubusercontent.com/openai/codex/' + release['commit'] + '/' + notice,
                          stage / notice)
                # Rename only after extraction, digest and attribution have succeeded.
                os.rename(stage, published)
        return verified_binary(published, assets)
    finally:
        lock.rmdir()


def remember(cache, release, assets):
    """Record the provisioned release so offline readers find it without the network."""
    record = {'tag': release['tag'], 'commit': release['commit'],
              'assets': {archive: expected for archive, expected, _, _, _ in assets}}
    with tempfile.NamedTemporaryFile('w', dir=cache, prefix='current-', delete=False) as target:
        json.dump(record, target)
    os.replace(target.name, cache / POINTER)


def remembered(cache):
    try:
        record = json.loads((cache / POINTER).read_text())
        commit, tag = record['commit'], record['tag']
        assets = host_assets(record['assets'])
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise ValueError('binary missing; provision before testing') from error
    requested = os.environ.get('CODEX_RELEASE', '').strip()
    if not re.fullmatch(r'[0-9a-f]{40}', str(commit)) or (requested and requested != tag):
        raise ValueError('binary missing; provision before testing')
    published = published_directory(cache, commit, assets)
    if not published.exists():
        raise ValueError('binary missing; provision before testing')
    return published, assets


def host(root):
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'AMD64'):
        raise ValueError('unsupported host; requires Linux x86_64')
    cache = cache_directory(root)
    if os.environ.get('ECHO_CODEX_OFFLINE') == '1':
        published, assets = remembered(cache)
        return verified_binary(published, assets)
    release = resolve(root)
    assets = host_assets(release['assets'])
    published = published_directory(cache, release['commit'], assets)
    binary = verified_binary(published, assets) if published.exists() else publish(published, release, assets)
    remember(cache, release, assets)
    print('codex-app-server: ' + release['tag'] + ' ' + release['commit'] + ' '
          + ' '.join('sha256:' + expected for _, expected, _, _, _ in assets), file=sys.stderr)
    return binary


def main(arguments):
    if len(arguments) != 2 or arguments[0] not in ('resolve', 'host'):
        raise ValueError('usage: codex_release.py resolve|host <repository root>')
    root = pathlib.Path(arguments[1]).resolve()
    if arguments[0] == 'host':
        print(host(root))
        return
    release = resolve(root)
    for key in ('tag', 'version', 'commit'):
        print(key + '=' + release[key])


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except Exception as error:  # every failure is one diagnostic line and exit 1
        print('codex-app-server: ' + str(error), file=sys.stderr)
        sys.exit(1)
