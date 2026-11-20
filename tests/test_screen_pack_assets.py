"""T3 artifact witness: original generated media, local processes, no network/device."""
import hashlib
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile

from test_echo_install import require, SENTINEL

ROOT = Path(__file__).resolve().parents[1]
STATES = {'idle', 'connecting', 'listening', 'thinking', 'speaking', 'muted',
          'setup', 'approval', 'error'}


def generate(out, version='0.1.0', name='template'):
    command = ['sh', str(ROOT / 'scripts/make-template-screenpack.sh'),
               '--version', version, '--name', name, '--out', str(out)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=120)
    require(result.returncode == 0, 'template generation failed: ' + result.stderr)
    require(result.stdout == f'screenpack: {name}={version} generated=PASS checksum=PASS\n',
            'generator success diagnostic differs')
    rejected = subprocess.run(command, capture_output=True, text=True, timeout=10)
    require(rejected.returncode == 1 and rejected.stdout == '', 'existing output was replaced')
    require(rejected.stderr == 'screenpack: generation failed; output was not published\n',
            'unsafe generator diagnostic')
    command[command.index('--version') + 1] = '../escape'
    rejected = subprocess.run(command, capture_output=True, text=True, timeout=10)
    require(rejected.returncode == 2, 'invalid version was accepted')
    independent_template(out / f'{name}-{version}.screenpack.tar.gz', name)
    if name == 'default':
        from screen_pack_face import face_cases
        face_cases(out / f'{name}@{version}', decode)
    elif version == '0.2.0':
        from screen_pack_eyes import eyes_cases
        eyes_cases(out / f'template@{version}', decode)
    else:
        media_cases(out / f'template@{version}')
    print('F23 template: identity=PASS archive=PASS checksum=PASS codecs=PASS rest_boundaries=PASS')


def independent_template(archive, name='template'):
    # The default face adds the awake (wake) clip pair and its pack.css.
    states = STATES | {'wake'} if name == 'default' else STATES
    count = 2 + 2 * (len(states) + 1) + (name == 'default')
    with tarfile.open(archive) as source:
        members = source.getmembers()
        require(len(members) == count and all(m.isfile() for m in members), 'template membership differs')
        files = {m.name: source.extractfile(m).read() for m in members}
        require(len(files) == count, 'duplicate template member')
    manifest = json.loads(files['pack.json'])
    require(manifest['format'] == 1 and manifest['name'] == name, 'template identity differs')
    require(manifest['version'] == archive.name[len(name) + 1:-18], 'template version differs')
    require(manifest['character']['rest'] == 'rest.png', 'rest reference differs')
    require(set(manifest['character']['states']) == states, 'mapped states differ')
    if tuple(map(int, manifest['version'].split('-', 1)[0].split('.')[:2])) < (0, 2):
        require(manifest['layout'] == {
            'safe_area': {'x': 0, 'y': 0, 'width': 100, 'height': 100}, 'slots': {},
            'stage': {'anchor': {'x': 50, 'y': 50}, 'scale': 1, 'background': '#101820'}},
            'default layout differs')
    expected = {'pack.json', 'rest.png'} | ({'pack.css'} if name == 'default' else set())
    for clip_name in sorted(states | {'happy'}):
        clip = (manifest['character']['emotes']['happy'] if clip_name == 'happy'
                else manifest['character']['states'][clip_name]['clip'])
        require(clip == {'webm': clip_name + '.webm', 'mp4': clip_name + '.mp4',
                         'mode': 'once' if clip_name == 'happy' else 'loop'}, 'clip mapping differs')
        expected.update((clip_name + '.webm', clip_name + '.mp4'))
    require(set(files) == expected, 'unexpected template payload')
    require(sum(map(len, files.values())) <= 64 * 1024**2, 'expanded limit exceeded')
    require(all(len(data) <= 8 * 1024**2 for name, data in files.items()
                if name.endswith(('.webm', '.mp4'))), 'clip limit exceeded')
    require(files['rest.png'].startswith(b'\x89PNG\r\n\x1a\n'), 'rest is not PNG')
    sidecar = archive.with_name(archive.name + '.sha256')
    if sidecar.exists():
        require(sidecar.read_text() == hashlib.sha256(archive.read_bytes()).hexdigest()
                + '  ' + archive.name + '\n', 'generator checksum differs')
    return files


def decode(path, frames, alpha=False):
    command = ['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error']
    if alpha:
        command += ['-c:v', 'libvpx-vp9']
    command += ['-i', str(path), '-vf', frames, '-fps_mode', 'passthrough',
                '-f', 'rawvideo', '-pix_fmt', 'rgba', '-threads', '1', 'pipe:1']
    return subprocess.check_output(command, stderr=subprocess.PIPE, timeout=30)


def independent_bundle(assets, pack, asset_names):
    archive = next(assets.glob('echo-*-armv7.tar.gz'))
    with tarfile.open(archive) as bundle:
        members = bundle.getmembers()
        require(all(m.isfile() for m in members), 'non-file payload')
        files = {m.name: bundle.extractfile(m).read() for m in members}
        require(len(files) == len(members), 'duplicate member')
    manifest = json.loads(files.pop('MANIFEST'))
    require(set(files) == set(manifest['files']), 'manifest membership differs')
    for name, content in files.items():
        require(hashlib.sha256(content).hexdigest() == manifest['files'][name], 'payload hash differs')
        require(pack.safe_name(name), 'forbidden member')
    template = assets / ('template-' + manifest['version'] + '.screenpack.tar.gz')
    art = independent_template(template)
    require(all(data not in files['bin/echo-bridge'] for data in art.values()), 'art embedded in bridge')
    expected = {name: hashlib.sha256((assets / name).read_bytes()).hexdigest()
                for name in asset_names(manifest['version'])}
    lines = (assets / 'SHA256SUMS').read_text().splitlines()
    require(len(lines) == len(expected) and set(lines) == {
        value + '  ' + name for name, value in expected.items()}, 'asset hashes differ')
    config = pack.tomllib.loads(files['examples/bridge.echo.toml'].decode())
    require(config['screen']['packs'] == [{
        'url': f"https://github.com/portpowered/portos-echo/releases/download/v{manifest['version']}/{template.name}",
        'sha256': expected[template.name]}], 'template release pin differs')
    require(manifest['components']['ca_bundle']['sha256'] == hashlib.sha256(files['bin/cacert.pem']).hexdigest(),
            'CA provenance differs')
    pack.verify(archive, assets / 'SHA256SUMS')
    return manifest


def exclusion_cases(archive, pack):
    art = independent_template(archive)
    for name, data in art.items():
        try:
            pack.exclude_template(art, (b'synthetic-executable', b'prefix' + data + b'suffix'))
        except pack.BundleError as error:
            require(str(error) == 'template data embedded in executable or JavaScript',
                    'media exclusion diagnostic differs')
        else:
            raise AssertionError('embedded generated payload accepted: ' + name)
    pack.exclude_template(art, (b'synthetic-executable', b'synthetic-javascript'))
    print('U-PACK complete generated payload exclusion: PASS')


def media_cases(pack):
    neutral = decode(pack / 'rest.png', 'null')
    require(len(neutral) == 640 * 640 * 4, 'rest dimensions differ')
    reference = None
    for name in sorted(STATES | {'happy'}):
        for extension, codec in (('webm', 'vp9'), ('mp4', 'h264')):
            path = pack / (name + '.' + extension)
            info = json.loads(subprocess.check_output([
                'ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(path)], timeout=10))
            require(len(info['streams']) == 1, 'pack clip contains audio or extra stream')
            stream = info['streams'][0]
            require((stream['codec_name'], stream['width'], stream['height'], stream['r_frame_rate'])
                    == (codec, 640, 640, '30/1'), 'codec/dimensions/fps differ')
            raw = decode(path, 'select=eq(n\\,0)+eq(n\\,15)+eq(n\\,30)', extension == 'webm')
            size = len(neutral)
            require(len(raw) == size * 3, 'expected 31 video frames')
            first, middle, last = raw[:size], raw[size:2 * size], raw[2 * size:]
            require(first != middle, 'state does not animate')
            if extension == 'webm':
                tags = {key.lower(): value for key, value in stream['tags'].items()}
                require(tags.get('alpha_mode') == '1', 'VP9 alpha metadata absent: ' + json.dumps(tags))
                require(first == last, 'VP9 loop does not join on rest')
                require(first[3] == 0 and first[(290 * 640 + 320) * 4 + 3] == 255,
                        'VP9 alpha did not decode')
                require(first == reference if reference else True, 'clips have different rest poses')
                reference = first
                # Neutral image and encoded video share geometry/alpha; YUV changes color slightly.
                require(first[3::4] == neutral[3::4], 'rest/video alpha geometry differs')
            else:
                require(first[3] == 255, 'MP4 fallback must be opaque')
                require(all(abs(a - b) < 5 for a, b in zip(first[:3], (16, 24, 32))),
                        'MP4 fallback stage background differs')
                require(sum(abs(a - b) for a, b in zip(first, last)) / size < 1,
                        'H264 loop boundary differs from neutral')


REMOTE = '/data/local/echo-bridge/home/screens'


def push_adb():
    """T3 boundary: execute real shell/filesystem operations in an isolated fake device."""
    base = Path(os.environ['PACK_FAKE_ROOT'])
    args = sys.argv[2:]
    with (base / 'ledger').open('a') as ledger:
        ledger.write(json.dumps(args) + '\n')
    # A failed transport must never disclose remote stdout/stderr or a local path.
    print(SENTINEL)
    print(SENTINEL, file=sys.stderr)
    remote = base / 'device/data/local/echo-bridge/home/screens'
    if args[0] == 'push':
        count = int((base / 'pushes').read_text()) + 1
        (base / 'pushes').write_text(str(count))
        if os.environ.get('PACK_HOLD_PUSH') and count == 2:
            with (base / 'push-ready').open('w') as ready:
                ready.write('ready')
            with (base / 'push-release').open('r') as release:
                release.read()
        if count == int(os.environ.get('PACK_FAIL_PUSH', '0')):
            print(f"adb: error: failed to copy '{args[1]}' for {SENTINEL}: Permission denied",
                  file=sys.stderr)
            return 1
        target = base / ('device' + args[2])
        require(str(target).startswith(str(remote) + '/.push-'), 'transfer escaped private stage')
        shutil.copyfile(args[1], target)
        return 0
    require(args[0] == 'shell' and len(args) == 2, 'unexpected adb authority')
    command = args[1]
    if any(line.startswith('mv ') for line in command.splitlines()):
        visible = sorted(p.name for p in remote.iterdir() if not p.name.startswith('.'))
        (base / 'visible-before-commit').write_text(json.dumps(visible))
        if os.environ.get('PACK_FAIL_COMMIT'):
            print(f"mv: {SENTINEL}: Read-only file system", file=sys.stderr)
            return 1
    if os.environ.get('PACK_FAIL_CLEANUP') and 'rm -rf ' in command:
        print(f"adb: lost transport for {SENTINEL}, {base}", file=sys.stderr)
        return 1
    command = command.replace('/data', str(base / 'device/data'))
    completed = subprocess.run(['sh', '-c', command], capture_output=True, timeout=10,
                               env=dict(os.environ, PATH=str(base / 'tools') + ':/usr/bin:/bin'))
    sys.stderr.buffer.write(completed.stderr)
    return completed.returncode


def toybox_move():
    args = sys.argv[2:]
    if any(arg.startswith('-') and 'T' in arg for arg in args):
        print('mv: Unknown option T', file=sys.stderr)
        return 1
    require(len(args) == 3 and args[0] == '-n', 'unexpected mv flags')
    source, destination = map(Path, args[1:])
    if os.environ.get('PACK_RACE'):
        # Publish after the helper's destination check but just before mv
        # resolves its leaf. A complete pack must never gain a nested stage.
        rival = Path(os.environ['PACK_FAKE_ROOT']) / ('device' + REMOTE) / 'template@0.1.0'
        rival.mkdir()
        (rival / 'pack.json').write_bytes((source / 'pack.json').read_bytes())
        (rival / 'nested').mkdir()
        (rival / 'nested/rest.png').write_bytes(b'concurrent publication')
    if destination.is_dir():
        destination /= source.name
    if not destination.exists() and not destination.is_symlink():
        source.rename(destination)
    return 0


class PushBox:
    def __enter__(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='pack-push-')
        self.base = Path(self.temporary.name)
        self.remote = self.base / ('device' + REMOTE)
        self.remote.parent.mkdir(parents=True)
        self.tools = self.base / 'tools'
        self.tools.mkdir()
        # This is the only adb discoverable by the helper, never a real device.
        for name in ('adb', 'mv'):
            tool = self.tools / name
            tool.write_text('#!/bin/sh\nexec ' + shlex.quote(sys.executable) + ' '
                            + shlex.quote(str(Path(__file__).resolve())) + f' --fake-{name} "$@"\n')
            tool.chmod(0o700)
        chown = self.tools / 'chown'
        chown.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$PACK_FAKE_ROOT/owners"\n')
        chown.chmod(0o700)
        (self.base / 'pushes').write_text('0')
        self.env = dict(os.environ, PACK_FAKE_ROOT=str(self.base),
                        PATH=str(self.tools) + ':/usr/bin:/bin')
        self.pack = self.base / 'source with spaces'
        self.pack.mkdir()
        self.manifest = {'format': 1, 'name': 'template', 'version': '0.1.0',
                         'character': {'rest': 'nested/rest.png'}}
        (self.pack / 'nested').mkdir()
        (self.pack / 'nested/rest.png').write_bytes(b'synthetic-still')
        self.write_manifest()
        return self

    def write_manifest(self):
        (self.pack / 'pack.json').write_text(json.dumps(self.manifest))

    def run(self, okay=True):
        result = subprocess.run(['sh', str(ROOT / 'scripts/echo-screen-pack.sh'),
                                 'push', str(self.pack)], env=self.env,
                                capture_output=True, text=True, timeout=30)
        if okay:
            require(result.returncode == 0 and result.stdout == f"screen-pack: pushed template@{self.manifest['version']}\n"
                    and result.stderr == '', 'push success diagnostic differs: ' + result.stderr)
        else:
            require(result.returncode == 1 and result.stdout == ''
                    and result.stderr.startswith('screen-pack: '), 'push failure diagnostic differs')
            require(SENTINEL not in result.stderr and str(self.base) not in result.stderr,
                    'push leaked private diagnostic')
        return result

    def ledger(self):
        path = self.base / 'ledger'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def settled(self, expected):
        require(sorted(path.name for path in self.remote.iterdir()) == expected,
                'staging/lock leaked or prior pack changed')

    def __exit__(self, *exception):
        self.temporary.cleanup()


def push_cases(generated, version='0.1.0', echo_home=None):
    with PushBox() as box:
        identity = f'template@{version}'
        box.manifest['version'] = version
        shutil.rmtree(box.pack)
        shutil.copytree(generated / identity, box.pack)
        prior = []
        if version != '0.1.0':
            prior = ['template@0.1.0']
            box.remote.mkdir()
            shutil.copytree(generated / 'older/template@0.1.0', box.remote / prior[0])
        box.run()
        target = box.remote / identity
        expected = {p.relative_to(box.pack).as_posix(): p.read_bytes() for p in box.pack.rglob('*') if p.is_file()}
        actual = {p.relative_to(target).as_posix(): p.read_bytes() for p in target.rglob('*') if p.is_file()}
        require(actual == expected and len(actual) == 22, 'published generated bytes differ')
        require(json.loads((box.base / 'visible-before-commit').read_text()) == prior, 'partial pack was visible')
        require(all(p.stat().st_mode & 0o777 == (0o700 if p.is_dir() else 0o600)
                    for p in [target, *target.rglob('*')]), 'private pack permissions differ')
        require(all('9999:9999' in line for line in (box.base / 'owners').read_text().splitlines()),
                'pack ownership differs from bridge home owner')
        box.run(okay=False)
        require({p.name: p.read_bytes() for p in target.iterdir()} == actual,
                'duplicate publication replaced original pack')
        box.settled([*prior, identity])
        if echo_home is not None:
            shutil.copytree(box.remote, echo_home / 'screens')
    failures = {
        'PACK_FAIL_PUSH': 'transfer failed: Permission denied',
        'PACK_FAIL_COMMIT': 'publication failed: Read-only file system',
        'PACK_RACE': 'publication failed: diagnostic redacted',
    }
    for fault, expected in failures.items():
        with PushBox() as box:
            box.remote.mkdir()
            prior = box.remote / 'prior@1.0.0'
            prior.mkdir()
            (prior / 'rest.png').write_bytes(b'prior-active-data')
            box.env[fault] = '2' if fault == 'PACK_FAIL_PUSH' else '1'
            result = box.run(okay=False)
            require(result.stderr == f'screen-pack: adb {expected}\n', 'failure step or reason differs')
            box.settled(['prior@1.0.0', 'template@0.1.0'] if fault == 'PACK_RACE' else ['prior@1.0.0'])
            require((prior / 'rest.png').read_bytes() == b'prior-active-data', 'failed push changed active data')
            if fault == 'PACK_RACE':
                rival = box.remote / 'template@0.1.0'
                require(sorted(p.relative_to(rival).as_posix() for p in rival.rglob('*'))
                        == ['nested', 'nested/rest.png', 'pack.json'],
                        'competing pack gained unexpected data')
                require((rival / 'nested/rest.png').read_bytes() == b'concurrent publication',
                        'no-clobber publication lost a racing identity')
    for ancestor in ('screens', 'home'):
        with PushBox() as box:
            escape = box.base / 'outside'
            escape.mkdir()
            victim = box.remote if ancestor == 'screens' else box.remote.parent
            if victim.exists():
                victim.rmdir()
            victim.symlink_to(escape, target_is_directory=True)
            box.run(okay=False)
            require(list(escape.iterdir()) == [], 'remote symlink escape was written')
            require(not any(call[0] == 'push' for call in box.ledger()), 'symlink refusal happened after transfer')
    with PushBox() as box:
        box.remote.mkdir()
        (box.remote / '.push-lock').mkdir()
        box.run(okay=False)
        box.settled(['.push-lock'])
    invalid_push_cases()
    interrupted_push_case()
    cleanup_failure_cases()
    print('F24 local push: generated_bytes=PASS atomic=PASS duplicate=REFUSED failure_retention=PASS cleanup=PASS symlinks=REFUSED')
    print('F24 toybox: publication=PASS competing_identity=RETAINED diagnostics=SANITIZED cleanup_precedence=PASS')


def cleanup_failure_cases():
    for fail_transfer in (False, True):
        with PushBox() as box:
            box.env['PACK_FAIL_CLEANUP'] = '1'
            if fail_transfer:
                box.env['PACK_FAIL_PUSH'] = '2'
            result = box.run(okay=False)
            expected = 'screen-pack: adb cleanup failed: diagnostic redacted\n'
            if fail_transfer:
                expected = 'screen-pack: adb transfer failed: Permission denied\n' + expected
            require(result.stderr == expected, 'cleanup failure obscured the primary result')
            stages = list(box.remote.glob('.push-*'))
            require(len(stages) == 2 and all(path.is_dir() for path in stages)
                    and (box.remote / '.push-lock').is_dir(), 'uncertain staging or lock was lost')
            target = box.remote / 'template@0.1.0/nested/rest.png'
            require(target.exists() != fail_transfer, 'cleanup changed publication status')
            if target.exists():
                require(target.read_bytes() == b'synthetic-still', 'cleanup damaged published art')


def invalid_push_cases():
    mutations = {
        'name': lambda b: b.manifest.update(name="bad'; touch /tmp/escape"),
        'version': lambda b: b.manifest.update(version='../escape'),
        'format': lambda b: b.manifest.update(format=True),
        'unknown': lambda b: b.manifest.update(script='run'),
        'null': lambda b: b.manifest.update(character=None),
        'reference': lambda b: b.manifest['character'].update(rest='../rest.png'),
        'missing': lambda b: b.manifest['character'].update(rest='absent.png'),
        'state': lambda b: b.manifest['character'].update(states={'unknown': {}}),
        'clip': lambda b: b.manifest['character'].update(emotes={'happy': {'mode': 'loop'}}),
        'symlink': lambda b: (b.pack / 'link.png').symlink_to(b.pack / 'nested/rest.png'),
        'hardlink': lambda b: os.link(b.pack / 'nested/rest.png', b.pack / 'link.png'),
        'fifo': lambda b: os.mkfifo(b.pack / 'pipe.png'),
        'type': lambda b: (b.pack / 'code.sh').write_text('exit 0'),
        'css-remote': lambda b: (b.pack / 'style.css').write_text('.pack-stage {background-image:url(https://invalid.test/rest.png)}'),
        'css-import': lambda b: (b.pack / 'style.css').write_text('@import "x";'),
        'css-expression': lambda b: (b.pack / 'style.css').write_text('.pack-stage {opacity:expression(1)}'),
        'css-escape': lambda b: (b.pack / 'style.css').write_text('.pack-stage {background-image:u\\72l(x)}'),
    }
    for mutation in mutations.values():
        with PushBox() as box:
            mutation(box)
            box.write_manifest()
            box.run(okay=False)
            require(box.ledger() == [], 'invalid pack reached adb')
    for cap in ('clip', 'count', 'expanded'):
        with PushBox() as box:
            count = 65 if cap == 'count' else 1
            for index in range(count):
                path = box.pack / (str(index) + ('.webm' if cap == 'clip' else '.png'))
                with path.open('wb') as file:
                    file.truncate(8 * 1024**2 + 1 if cap == 'clip' else 64 * 1024**2 + 1 if cap == 'expanded' else 0)
            box.run(okay=False)
            require(box.ledger() == [], 'over-limit pack reached adb')
    with PushBox() as box:
        (box.pack / 'pack.json').write_text('{malformed')
        box.run(okay=False)
        require(box.ledger() == [], 'malformed JSON reached adb')
    with PushBox() as box:
        (box.pack / 'pack.json').write_text('{"format":1,"name":"one","name":"two"}')
        result = box.run(okay=False)
        require(result.stderr == 'screen-pack: duplicate pack.json field\n' and box.ledger() == [],
                'duplicate JSON field reached adb')
    with PushBox() as box:
        (box.pack / 'style.css').write_text('.pack-stage { background-image: url("nested/rest.png"); opacity: 0.5; }')
        box.run()
        box.settled(['template@0.1.0'])
    print('F24 preflight: identity=PASS schema=PASS references=PASS limits=PASS inert_CSS=PASS invalid_adb_calls=0')


def interrupted_push_case():
    with PushBox() as box:
        os.mkfifo(box.base / 'push-ready')
        os.mkfifo(box.base / 'push-release')
        box.env['PACK_HOLD_PUSH'] = '1'
        ready = os.open(box.base / 'push-ready', os.O_RDONLY | os.O_NONBLOCK)
        try:
            with subprocess.Popen(['sh', str(ROOT / 'scripts/echo-screen-pack.sh'),
                                   'push', str(box.pack)], env=box.env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
                with selectors.DefaultSelector() as selector:
                    selector.register(ready, selectors.EVENT_READ)
                    require(bool(selector.select(15)), 'transfer did not reach interruption barrier')
                require(os.read(ready, 5) == b'ready', 'transfer readiness differs')
                process.send_signal(signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=15)
                require(process.returncode == 1 and stdout == b''
                        and stderr == b'screen-pack: push interrupted\n', 'interrupted push diagnostic differs')
        finally:
            os.close(ready)
        box.settled([])
        require(int((box.base / 'pushes').read_text()) == 2, 'interrupted transfer continued')
    print('F24 cancellation: interrupted=PASS cleanup=PASS publication=0')


if __name__ == '__main__':
    if sys.argv[1:2] == ['--fake-adb']:
        sys.exit(push_adb())
    if sys.argv[1:2] == ['--fake-mv']:
        sys.exit(toybox_move())
    if sys.argv[1:2] == ['--template-witness'] and len(sys.argv) == 3:
        from screen_pack_eyes import contact_sheet
        destination = Path(sys.argv[2]).resolve()
        generate(destination, '0.2.0')
        generate(destination / 'older')
        generate(destination / 'default', '0.3.0', 'default')
        contact_sheet(destination / 'template@0.2.0', destination / 'contact.png', decode)
        push_cases(destination, '0.2.0', destination / 'pushed-home')
