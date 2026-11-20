"""T3 artifact witness: original generated media, local processes, no network/device."""
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

from test_echo_install import require

ROOT = Path(__file__).resolve().parents[1]
STATES = {'idle', 'connecting', 'listening', 'thinking', 'speaking', 'muted',
          'setup', 'approval', 'error'}


def generate(out):
    command = ['sh', str(ROOT / 'scripts/make-template-screenpack.sh'),
               '--version', '0.1.0', '--out', str(out)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=120)
    require(result.returncode == 0, 'template generation failed: ' + result.stderr)
    require(result.stdout == 'screenpack: template=0.1.0 generated=PASS checksum=PASS\n',
            'generator success diagnostic differs')
    rejected = subprocess.run(command, capture_output=True, text=True, timeout=10)
    require(rejected.returncode == 1 and rejected.stdout == '', 'existing output was replaced')
    require(rejected.stderr == 'screenpack: generation failed; output was not published\n',
            'unsafe generator diagnostic')
    command[command.index('--version') + 1] = '../escape'
    rejected = subprocess.run(command, capture_output=True, text=True, timeout=10)
    require(rejected.returncode == 2, 'invalid version was accepted')
    independent_template(out / 'template-0.1.0.screenpack.tar.gz')
    media_cases(out / 'template@0.1.0')
    print('F23 template: identity=PASS archive=PASS checksum=PASS codecs=PASS rest_boundaries=PASS')


def independent_template(archive):
    with tarfile.open(archive) as source:
        members = source.getmembers()
        require(len(members) == 22 and all(m.isfile() for m in members), 'template membership differs')
        files = {m.name: source.extractfile(m).read() for m in members}
        require(len(files) == 22, 'duplicate template member')
    manifest = json.loads(files['pack.json'])
    require(manifest['format'] == 1 and manifest['name'] == 'template', 'template identity differs')
    require(manifest['version'] == archive.name[9:-18], 'template version differs')
    require(manifest['character']['rest'] == 'rest.png', 'rest reference differs')
    require(set(manifest['character']['states']) == STATES, 'mapped states differ')
    require(manifest['layout'] == {
        'safe_area': {'x': 0, 'y': 0, 'width': 100, 'height': 100}, 'slots': {},
        'stage': {'anchor': {'x': 50, 'y': 50}, 'scale': 1, 'background': '#101820'}},
        'default layout differs')
    expected = {'pack.json', 'rest.png'}
    for name in sorted(STATES | {'happy'}):
        clip = (manifest['character']['emotes']['happy'] if name == 'happy'
                else manifest['character']['states'][name]['clip'])
        require(clip == {'webm': name + '.webm', 'mp4': name + '.mp4',
                         'mode': 'once' if name == 'happy' else 'loop'}, 'clip mapping differs')
        expected.update((name + '.webm', name + '.mp4'))
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
                '-f', 'rawvideo', '-pix_fmt', 'rgba', 'pipe:1']
    return subprocess.check_output(command, stderr=subprocess.PIPE, timeout=15)


def independent_bundle(assets, pack):
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
    expected = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                for path in (archive, assets / 'install.sh', template)}
    hashes = {name: value for value, name in
              (line.split() for line in (assets / 'SHA256SUMS').read_text().splitlines())}
    require(hashes == expected, 'asset hashes differ')
    config = pack.tomllib.loads(files['examples/bridge.echo.toml'].decode())
    require(config['screen']['packs'] == [{
        'url': f"https://github.com/portpowered/portos-echo/releases/download/v{manifest['version']}/{template.name}",
        'sha256': expected[template.name]}], 'template release pin differs')
    require(manifest['components']['ca_bundle']['sha256'] == hashlib.sha256(files['bin/cacert.pem']).hexdigest(),
            'CA provenance differs')
    pack.verify(archive, assets / 'SHA256SUMS')


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
