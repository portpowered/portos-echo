#!/bin/sh
# Original procedural art only. Linux host tooling; no downloads or device access.
set -eu
exec python3 - "$@" <<'PY'
import argparse
import binascii
import gzip
import hashlib
import io
import json
import math
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import zlib


def rectangle(frame, x, y, width, height, color):
    row = bytes(color) * width
    for line in range(y, y + height):
        start = (line * 640 + x) * 4
        frame[start:start + len(row)] = row


def ellipse(frame, x, y, rx, ry, color):
    for line in range(-ry, ry + 1):
        half = int(rx * math.sqrt(max(0, 1 - (line / ry) ** 2)))
        rectangle(frame, x - half, y + line, half * 2 + 1, 1, color)


def picture(color, amount):
    # Transparent pixels retain the MP4 stage background when alpha is dropped.
    frame = bytearray(bytes((16, 24, 32, 0)) * (640 * 640))
    neutral = (160, 210, 230)
    body = tuple(round(a + (b - a) * amount) for a, b in zip(neutral, color)) + (255,)
    ellipse(frame, 320, 290, 180, 160, body)
    for x in (255, 385):
        ellipse(frame, x, 260, 19, 24, (16, 24, 32, 255))
    ellipse(frame, 320, 345, 48, 8 + round(24 * amount), (16, 24, 32, 255))
    # A tiny original bitmap alphabet labels the common REST pose without fonts.
    glyphs = ('110/101/110/101/101', '111/100/110/100/111',
              '111/100/111/001/111', '111/010/010/010/010')
    for index, glyph in enumerate(glyphs):
        for y, row in enumerate(glyph.split('/')):
            for x, bit in enumerate(row):
                if bit == '1':
                    rectangle(frame, 263 + index * 30 + x * 7, 490 + y * 7,
                              7, 7, (240, 248, 255, 255))
    return bytes(frame)


def png(raw):
    def chunk(kind, content):
        return (struct.pack('!I', len(content)) + kind + content
                + struct.pack('!I', binascii.crc32(kind + content)))
    scanlines = b''.join(b'\0' + raw[start:start + 2560]
                         for start in range(0, len(raw), 2560))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', 640, 640, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(scanlines)) + chunk(b'IEND', b''))


def generate(stage, work, version):
    pack = stage / ('template@' + version)
    pack.mkdir()
    rest = picture((160, 210, 230), 0)
    (pack / 'rest.png').write_bytes(png(rest))
    colors = {
        'idle': (100, 200, 220), 'connecting': (160, 160, 245),
        'listening': (100, 235, 180), 'thinking': (205, 160, 240),
        'speaking': (100, 185, 255), 'muted': (175, 180, 185),
        'setup': (245, 210, 110), 'approval': (255, 190, 100),
        'error': (255, 110, 120), 'happy': (255, 210, 120),
    }
    for name, color in colors.items():
        raw = work / 'frames.rgba'
        with raw.open('wb') as frames:
            for number in range(31):
                # Exact equal endpoints; derivative zero at both neutral joins.
                amount = 0 if number in (0, 30) else math.sin(math.pi * number / 30) ** 2
                frames.write(rest if amount == 0 else picture(color, amount))
        common = ['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error',
                  '-f', 'rawvideo', '-pixel_format', 'rgba', '-video_size', '640x640',
                  '-framerate', '30', '-i', str(raw), '-an', '-threads', '1']
        subprocess.run([*common, '-c:v', 'libvpx-vp9', '-pix_fmt', 'yuva420p',
                        '-auto-alt-ref', '0', '-lossless', '1', '-metadata:s:v:0',
                        'alpha_mode=1', str(pack / (name + '.webm'))],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        subprocess.run([*common, '-c:v', 'libx264', '-pix_fmt', 'yuv420p',
                        '-crf', '18', '-movflags', '+faststart', str(pack / (name + '.mp4'))],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    def clip(name, mode='loop'):
        return {'webm': name + '.webm', 'mp4': name + '.mp4', 'mode': mode}
    manifest = {
        'format': 1, 'name': 'template', 'version': version,
        'layout': {
            'safe_area': {'x': 0, 'y': 0, 'width': 100, 'height': 100},
            'slots': {},
            'stage': {'anchor': {'x': 50, 'y': 50}, 'scale': 1, 'background': '#101820'},
        },
        'character': {'rest': 'rest.png',
                      'states': {name: {'clip': clip(name)} for name in colors if name != 'happy'},
                      'emotes': {'happy': clip('happy', 'once')}},
    }
    (pack / 'pack.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    files = sorted(pack.iterdir())
    if (len(files) > 64 or sum(path.stat().st_size for path in files) > 64 * 1024**2
            or any(path.stat().st_size > 8 * 1024**2 for path in files if path.suffix in ('.webm', '.mp4'))):
        raise ValueError('generated pack exceeds limits')
    archive = stage / ('template-' + version + '.screenpack.tar.gz')
    # Regular USTAR entries at the archive root; fixed metadata, no path leakage.
    with archive.open('wb') as output, gzip.GzipFile(filename='', mode='wb', fileobj=output, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as target:
            for path in files:
                data = path.read_bytes()
                entry = tarfile.TarInfo(path.name)
                entry.mode = 0o644
                entry.size = len(data)
                target.addfile(entry, io.BytesIO(data))
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    (stage / (archive.name + '.sha256')).write_text(checksum + '  ' + archive.name + '\n', encoding='ascii')


def main():
    parser = argparse.ArgumentParser(description='Generate original template screen-pack data (Linux only).')
    parser.add_argument('--version', required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*)?', args.version):
        parser.error('invalid version')
    if sys.platform != 'linux' or not shutil.which('ffmpeg'):
        raise ValueError('Linux and ffmpeg are required')
    out = args.out.absolute()
    if out.exists() or out.is_symlink():
        raise ValueError('output already exists')
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.template-', dir=out.parent) as stage, tempfile.TemporaryDirectory() as work:
        generate(Path(stage), Path(work), args.version)
        os.rename(stage, out)
    print('screenpack: template=' + args.version + ' generated=PASS checksum=PASS')


try:
    main()
except (OSError, ValueError, subprocess.SubprocessError):
    print('screenpack: generation failed; output was not published', file=sys.stderr)
    sys.exit(1)
PY
