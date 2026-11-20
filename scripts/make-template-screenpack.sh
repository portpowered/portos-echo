#!/bin/sh
# Original procedural art only. Linux host tooling; no downloads or device access.
set -eu
exec python3 - "$@" <<'PY'
import argparse
import binascii
import concurrent.futures
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


EYE_SIZE = (640, 400)
BACKGROUND = (16, 24, 39)
PALE = (220, 235, 255)
IDLE_PATH = ((0, 0, 0), (52, 0, 0), (76, -124, -44), (135, -124, -44),
             (163, -45, 63), (216, -45, 63), (247, 124, 18), (310, 124, 18),
             (338, 76, -63), (392, 76, -63), (423, 0, 0), (479, 0, 0))
BLINKS = (99, 192, 366)
BLINK_SHAPE = (0.75, 0.35, 0.08, 0.32, 0.72)
SPEECH = ((0, 0), (3, 0), (7, 11), (12, 2), (14, 0), (21, 6), (25, 0),
          (28, 0), (31, 16), (37, 4), (40, 9), (46, 0), (52, 0), (55, 12),
          (60, 2), (64, 0), (70, 0))
FRAME_COUNTS = {'idle': 480, 'speaking': 71, 'thinking': 91, 'happy': 43}
EYE_COLORS = {
    'idle': PALE, 'connecting': (156, 174, 255), 'listening': (111, 235, 255),
    'thinking': (146, 207, 255), 'speaking': (209, 236, 255), 'muted': (255, 60, 75),
    'setup': (255, 204, 139), 'approval': (255, 230, 171), 'error': (255, 119, 76),
    'happy': PALE,
}


def interpolate(keys, frame):
    for (start, *left), (end, *right) in zip(keys, keys[1:]):
        if frame <= end:
            t = (frame - start) / (end - start)
            t = t * t * (3 - 2 * t)
            return tuple(a + (b - a) * t for a, b in zip(left, right))
    return tuple(keys[-1][1:])


def rounded(frame, x, y, width, height, radius, color, arch=0, size=EYE_SIZE):
    stride = size[0] * 4
    pixel = bytes(color)
    if not arch:
        for dy in range(-height, height + 1):
            corner = max(0, abs(dy) - height + radius)
            half = width - radius + round(math.sqrt(radius * radius - corner * corner))
            offset = (y + dy) * stride + (x - half) * 4
            frame[offset:offset + (2 * half + 1) * 4] = pixel * (2 * half + 1)
        return
    # The happy pose lifts the middle of each eye into an arch.
    for dx in range(-width, width + 1):
        corner = max(0, abs(dx) - width + radius)
        half = height - radius + round(math.sqrt(radius * radius - corner * corner))
        tip = y - round(arch * (1 - (dx / width) ** 2))
        for row in range(tip - half, tip + half + 1):
            offset = row * stride + (x + dx) * 4
            frame[offset:offset + 4] = pixel


def glow(frame, x, y, width, height, color, arch=0):
    radius = min(13, height)
    for spread, strength in ((12, 0.06), (8, 0.14), (4, 0.27)):
        tint = tuple(round(a + (b - a) * strength) for a, b in zip(BACKGROUND, color))
        rounded(frame, x, y, width + spread, height + spread,
                radius + spread, tint + (255,), arch)
    rounded(frame, x, y, width, height, radius, color + (255,), arch)


def eyes(name, number, last, still=False):
    frame = bytearray(bytes(BACKGROUND + (0,)) * math.prod(EYE_SIZE))
    width, height = 24, 48
    dx = dy = arch = 0
    phase = math.tau * number / last
    if name == 'idle':
        dx, dy = interpolate(IDLE_PATH, number)
        for blink in BLINKS:
            if blink <= number < blink + len(BLINK_SHAPE):
                height = round(height * BLINK_SHAPE[number - blink])
    elif name == 'listening':
        width, height = 26, 53
    elif name == 'thinking':
        dx, dy, height = 12 * math.sin(phase), -24, 28
    elif name == 'speaking':
        height = 43
    elif name == 'muted':
        height = 4
    elif name == 'error':
        height = 24
    elif name == 'connecting':
        dy = -9 * (1 - math.cos(phase))
    elif name == 'happy':
        smile, = interpolate(((0, 0), (10, 1), (22, 1), (34, 0), (last, 0)), number)
        height, arch = round(48 - 39 * smile), round(23 * smile)

    for center in (281, 359):
        if not still:
            # The page composites VP9 over the rest PNG. Cover its two eye
            # footprints so a shifted or narrowed pose fully replaces them.
            rounded(frame, center, 200, 36, 60, 25, BACKGROUND + (255,))
    for center in (281, 359):
        glow(frame, center + round(dx), 200 + round(dy), width, height, EYE_COLORS[name], arch)
    if name == 'speaking':
        opening, = interpolate(SPEECH, number)
        if opening >= 1:
            glow(frame, 320, 277, 19, round(opening), EYE_COLORS[name])
    return bytes(frame)


# Default 0.3 is a big friendly face that fills the 960 x 600 page: a full-page
# stage at one frame pixel per CSS pixel, two round eyes 240 CSS px across whose
# pair spans 65% of the width, and a mouth scaled to match. Frames are
# transparent outside the eyes and mouth; nothing covers the rest still.
FACE_SIZE = (960, 600)
FACE = {'half': 120, 'centers': (288, 672), 'y': 240, 'mouth': (445, 70, 5),
        'gaze': (0.8, 0.5), 'speech': 2.6}
# Card states (link code, approval, error) keep the face left of the card.
FACE_CARD = {'half': 90, 'centers': (158, 418)}
# A soft halo: many thin steps, so no ring shows at 1280 x 800.
FACE_GLOW = ((20, 0.03), (17, 0.06), (14, 0.09), (11, 0.13), (8, 0.18), (5, 0.24), (3, 0.30), (1, 0.36))
SESSION = ('wake', 'listening', 'thinking', 'speaking')
CARDS = ('setup', 'approval', 'error')
FACE_COUNTS = {'idle': 480, 'wake': 150, 'speaking': 71, 'thinking': 91, 'happy': 43}
FACE_COLORS = {**EYE_COLORS, 'speaking': PALE, 'wake': PALE}


def face_glow(frame, x, y, width, height, color, arch=0, radius=None):
    radius = min(width if radius is None else radius, height)
    spread = FACE_GLOW[0][0]
    if (x - width - spread < 0 or x + width + spread >= FACE_SIZE[0]
            or y - height - spread - arch < 0 or y + height + spread >= FACE_SIZE[1]):
        raise ValueError('face leaves the stage')
    for grow, strength in FACE_GLOW:
        tint = tuple(round(a + (b - a) * strength) for a, b in zip(BACKGROUND, color))
        rounded(frame, x, y, width + grow, height + grow, radius + grow, tint + (255,), arch, FACE_SIZE)
    rounded(frame, x, y, width, height, radius, color + (255,), arch, FACE_SIZE)


def face_pose(name, number, last):
    """Half width, half height, gaze offset and arch of each round eye."""
    half = FACE_CARD['half'] if name in CARDS else FACE['half']
    width = height = half
    dx = dy = arch = 0
    phase = math.tau * number / last
    if name == 'idle':
        dx, dy = (value * scale for value, scale in zip(interpolate(IDLE_PATH, number), FACE['gaze']))
        for blink in BLINKS:
            if blink <= number < blink + len(BLINK_SHAPE):
                height = round(half * BLINK_SHAPE[number - blink])
    elif name == 'wake' and 100 <= number < 100 + len(BLINK_SHAPE):
        height = round(half * BLINK_SHAPE[number - 100])
    elif name == 'listening':
        width = height = round(half * 1.08)
    elif name == 'thinking':
        dx, dy, height = 30 * math.sin(phase), -40, round(half * 0.6)
    elif name == 'muted':
        height = 12
    elif name == 'error':
        height = round(half * 0.55)
    elif name == 'connecting':
        dy = -20 * (1 - math.cos(phase))
    elif name == 'happy':
        smile, = interpolate(((0, 0), (10, 1), (22, 1), (34, 0), (last, 0)), number)
        height, arch = round(half - 0.8 * half * smile), round(0.45 * half * smile)
    return width, height, dx, dy, arch


def face(name, number, last):
    frame = bytearray(bytes(BACKGROUND + (0,)) * math.prod(FACE_SIZE))
    width, height, dx, dy, arch = face_pose(name, number, last)
    color = FACE_COLORS[name]
    centers = FACE_CARD['centers'] if name in CARDS else FACE['centers']
    for center in centers:
        # Round eyes: a blink or a lid squashes the circle vertically.
        face_glow(frame, center + round(dx), FACE['y'] + round(dy), width, height, color, arch)
    if name in SESSION:
        # Awake faces keep a closed mouth; only speaking opens it.
        y, half, closed = FACE['mouth']
        opening = interpolate(SPEECH, number)[0] * FACE['speech'] if name == 'speaking' else 0
        face_glow(frame, FACE_SIZE[0] // 2, y, half, max(closed, round(opening)), color)
    return bytes(frame)


def png(raw, size):
    def chunk(kind, content):
        return (struct.pack('!I', len(content)) + kind + content
                + struct.pack('!I', binascii.crc32(kind + content)))
    stride = size[0] * 4
    scanlines = b''.join(b'\0' + raw[start:start + stride] for start in range(0, len(raw), stride))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', *size, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(scanlines)) + chunk(b'IEND', b''))


def generate_clips(pack, work, style):
    colors = {
        'idle': (100, 200, 220), 'connecting': (160, 160, 245),
        'listening': (100, 235, 180), 'thinking': (205, 160, 240),
        'speaking': (100, 185, 255), 'muted': (175, 180, 185),
        'setup': (245, 210, 110), 'approval': (255, 190, 100),
        'error': (255, 110, 120), 'happy': (255, 210, 120),
    }
    if style >= 3:
        colors['wake'] = PALE
    size = FACE_SIZE if style >= 3 else EYE_SIZE if style >= 2 else (640, 640)
    counts = FACE_COUNTS if style >= 3 else FRAME_COUNTS
    if style >= 3:
        rest = face('idle', 0, 479)
    else:
        rest = eyes('idle', 0, 479, still=True) if style == 2 else picture((160, 210, 230), 0)
    (pack / 'rest.png').write_bytes(png(rest, size))

    def frames(name, color):
        count = counts.get(name, 31) if style >= 2 else 31
        for number in range(count):
            if style >= 3:
                yield face(name, number, count - 1)
            elif style == 2:
                yield eyes(name, number, count - 1)
            else:
                # Preserve prior release poses and exact neutral boundaries.
                amount = 0 if number in (0, 30) else math.sin(math.pi * number / 30) ** 2
                yield rest if amount == 0 else picture(color, amount)

    def encode(name):
        common = ['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error',
                  '-f', 'rawvideo', '-pixel_format', 'rgba', '-video_size', f'{size[0]}x{size[1]}',
                  '-framerate', '30', '-i', 'pipe:0', '-an', '-threads', '1']
        # Lossless VP9 decodes to the same pixels at any speed setting.
        speed = ['-deadline', 'good', '-cpu-used', '5'] if style >= 3 else []
        quality = ['-g', '1', '-qp', '18'] if style >= 2 and name == 'muted' else ['-crf', '18']
        commands = (
            [*common, '-c:v', 'libvpx-vp9', '-pix_fmt', 'yuva420p', *speed,
             '-auto-alt-ref', '0', '-lossless', '1', '-metadata:s:v:0',
             'alpha_mode=1', str(pack / (name + '.webm'))],
            [*common, '-c:v', 'libx264', '-pix_fmt', 'yuv420p',
             *quality, '-movflags', '+faststart', str(pack / (name + '.mp4'))])
        encoders = [subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL) for command in commands]
        try:
            for frame in frames(name, colors[name]):
                for encoder in encoders:
                    encoder.stdin.write(frame)
        finally:
            for encoder in encoders:
                encoder.stdin.close()
        for command, encoder in zip(commands, encoders):
            if encoder.wait():
                raise subprocess.CalledProcessError(encoder.returncode, command)

    # Each encoder is single-threaded and deterministic; clips encode in
    # parallel, longest first.
    order = sorted(colors, key=lambda name: -counts.get(name, 31))
    with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count() or 2) as pool:
        for done in [pool.submit(encode, name) for name in order]:
            done.result()
    return colors


def generate(stage, work, version, name='template'):
    pack = stage / (name + '@' + version)
    pack.mkdir()
    release = tuple(map(int, version.split('-', 1)[0].split('.')[:2]))
    style = 3 if release >= (0, 3) else 2 if release >= (0, 2) else 1
    names = generate_clips(pack, work, style)
    def clip(name, mode='loop'):
        return {'webm': name + '.webm', 'mp4': name + '.mp4', 'mode': mode}
    manifest = {
        'format': 1, 'name': name, 'version': version,
        'layout': {
            'safe_area': {'x': 0, 'y': 0, 'width': 100, 'height': 100},
            'slots': {},
            'stage': {'anchor': {'x': 50, 'y': 50}, 'scale': 1, 'background': '#101820'},
        },
        'character': {'rest': 'rest.png',
                      'states': {name: {'clip': clip(name)} for name in names if name != 'happy'},
                      'emotes': {'happy': clip('happy', 'once')}},
    }
    if style >= 2:
        manifest['layout']['slots'] = {
            'clock': {'x': 45, 'y': 86.5, 'font_scale': 1.1},
            'date': {'x': 44.8, 'y': 94, 'font_scale': 0.7},
            'status': {'x': 75, 'y': 17, 'font_scale': 0.9},
            **{state: {'x': 75, 'y': 24.5, 'font_scale': 0.7}
               for state in ('link_code', 'approval', 'error')},
        }
        manifest['layout']['stage'] = {
            'anchor': {'x': 37, 'y': 45}, 'scale': 1.45,
            'background': '#' + bytes(BACKGROUND).hex(),
        }
    if style >= 3:
        # The face fills the page; time and date share its centre line at the
        # bottom, and cards take the right 40% while the face moves left.
        slots = manifest['layout']['slots']
        slots['clock']['x'] = slots['date']['x'] = 50
        slots['status']['x'] = 60
        for state in ('link_code', 'approval', 'error'):
            slots[state]['x'] = 60
        manifest['layout']['stage'].update({'anchor': {'x': 50, 'y': 50}, 'scale': 2})
        manifest['layout']['css'] = 'pack.css'
        (pack / 'pack.css').write_text('.pack-clock, .pack-date { text-align: center; }\n',
                                       encoding='utf-8')
    (pack / 'pack.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    files = sorted(pack.iterdir())
    if (len(files) > 64 or sum(path.stat().st_size for path in files) > 64 * 1024**2
            or any(path.stat().st_size > 8 * 1024**2 for path in files if path.suffix in ('.webm', '.mp4'))):
        raise ValueError('generated pack exceeds limits')
    archive = stage / (name + '-' + version + '.screenpack.tar.gz')
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
    parser.add_argument('--name', choices=('template', 'default'), default='template')
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
        generate(Path(stage), Path(work), args.version, args.name)
        os.rename(stage, out)
    print('screenpack: ' + args.name + '=' + args.version + ' generated=PASS checksum=PASS')


try:
    main()
except (OSError, ValueError, subprocess.SubprocessError):
    print('screenpack: generation failed; output was not published', file=sys.stderr)
    sys.exit(1)
PY
