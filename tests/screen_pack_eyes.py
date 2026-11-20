"""Decoded-art and 960×600 layout checks for the generated eyes pack."""
from collections import Counter
from colorsys import rgb_to_hsv
from itertools import groupby
import json
from pathlib import Path
import re
import subprocess

from test_echo_install import require

SIZE = (640, 400)
SAMPLE = (160, 100)
SCREEN_CSS = Path(__file__).resolve().parents[1] / 'web/screen/src/screen.css'
SAMPLES = {'rest': 0, 'idle': 160, 'listening': 15, 'thinking': 22,
           'speaking': 31, 'muted': 15, 'connecting': 15, 'setup': 15,
           'approval': 15, 'error': 15, 'happy': 15}


def frames(raw, size):
    length = size[0] * size[1] * 4
    require(len(raw) % length == 0, 'partial decoded frame')
    return [raw[i:i + length] for i in range(0, len(raw), length)]


def pixel_runs(pack, name, decode, extension='webm', size=SAMPLE):
    path = pack / f'{name}.{extension}'
    return frames(decode(path, f'scale={size[0]}:{size[1]}', extension == 'webm'), size)


def lit(frame, start=0, end=SAMPLE[0] * SAMPLE[1]):
    return [i for i in range(start, end)
            if frame[i * 4 + 3] >= 128 and (frame[i * 4] > 145 or frame[i * 4 + 2] > 195)]


def centroid(pixels):
    return tuple(sum(p[i] for p in pixels) / len(pixels) for i in range(2))


def measure(frame):
    pixels = lit(frame)
    # Mode samples the bright core and ignores codec ringing at its edges.
    color = Counter(frame[i * 4:i * 4 + 3] for i in pixels).most_common(1)[0][0]
    center = centroid([divmod(i, SAMPLE[0])[::-1] for i in pixels])
    return center, len(pixels), color


def idle_and_focus(pack, decode):
    idle = pixel_runs(pack, 'idle', decode)
    require(360 <= len(idle) <= 600 and idle[0] == idle[-1], 'idle loop length or seam differs')
    centers, areas, colors = zip(*(measure(frame) for frame in idle))
    x, y = zip(*centers)
    span = (max(x) - min(x)) / SAMPLE[0]
    stable = {tuple(round(value) for value in center) for center, count in Counter(centers).items()
              if count >= 12}
    require(len(stable) >= 4 and span > .30 and max(y) - min(y) >= 20, 'idle gaze travel differs')
    require(max(abs(a - b) for a, b in zip(x, x[1:])) <= 3, 'idle gaze jumps between frames')
    blinks = [len(list(run)) for closed, run in groupby(area < max(areas) * .8 for area in areas) if closed]
    require(2 <= len(blinks) <= 3 and all(2 <= run <= 6 for run in blinks), 'idle blink timing differs')
    # Chroma subsampling and downscaling mix a blink's thin edges with the
    # backdrop. Compare the decoded core hues within that encoding tolerance.
    hues = [360 * rgb_to_hsv(*color)[0] for color in colors]
    require(205 <= min(hues) <= max(hues) <= 223 and max(hues) - min(hues) <= 8,
            f'idle eye hue varies from {min(hues):.1f} to {max(hues):.1f} degrees')
    for frame in pixel_runs(pack, 'listening', decode):
        (fx, fy), area, color = measure(frame)
        require(abs(fx - 79.5) < 3.2 and abs(fy - 49.5) < 2, 'listening gaze moved off center')
        require(area > max(areas) and color[1] - color[0] > 90, 'listening eyes lack cyan focus')
    require(not any(lit(frame, 85 * SAMPLE[0]) for frame in idle), 'idle shows unexpected marks')
    print(f'FACE idle: seconds={len(idle) / 30:g} stable_positions={len(stable)} '
          f'width_span={span:.1%} blink_frames={blinks} '
          f'hue={min(hues):.1f}–{max(hues):.1f}deg; listening=centered')


def muted_and_speaking(pack, decode):
    for extension in ('webm', 'mp4'):
        muted = pixel_runs(pack, 'muted', decode, extension, SIZE)
        require(len(muted) == 31 and all(frame == muted[0] for frame in muted),
                f'{extension} muted frames differ')
        stride = SIZE[0] * 4
        require(muted[0][200 * stride + 281 * 4] > 220
                and muted[0][200 * stride + 281 * 4 + 1] < 90
                and muted[0][185 * stride + 281 * 4] < 100, f'{extension} muted eyes are not red slits')
        speaking = pixel_runs(pack, 'speaking', decode, extension)
        counts = [len(lit(frame, 63 * SAMPLE[0])) for frame in speaking]
        periodic = any(counts[p:] == counts[:-p] for p in range(1, len(counts) // 2 + 1))
        require(counts[0] == counts[-1] == 0 and len(set(counts)) >= 3 and not periodic,
                f'{extension} mouth opening is static or periodic')
        print(f'FACE {extension}: muted_frames={len(muted)} identical red; '
              f'mouth_amplitudes={len(set(counts))} periodic={periodic}')


def clip_boundaries(pack, decode):
    for name in SAMPLES.keys() - {'rest'}:
        for extension, codec in (('webm', 'vp9'), ('mp4', 'h264')):
            path = pack / f'{name}.{extension}'
            info = json.loads(subprocess.check_output([
                'ffprobe', '-v', 'error', '-show_streams', '-of', 'json', str(path)], timeout=10))
            stream, = info['streams']
            require((stream['codec_name'], stream['width'], stream['height'], stream['r_frame_rate'])
                    == (codec, *SIZE, '30/1'), f'{name} codec, dimensions or rate differ')
            if extension == 'webm':
                tags = {key.lower(): value for key, value in stream['tags'].items()}
                require(tags.get('alpha_mode') == '1', 'VP9 alpha metadata absent')
            decoded = pixel_runs(pack, name, decode, extension)
            first, last = decoded[0], decoded[-1]
            require(first[3] == (0 if extension == 'webm' else 255), f'{name} alpha differs')
            if extension == 'mp4':
                require(all(abs(a - b) < 5 for a, b in zip(first[:3], (16, 24, 39))),
                        f'{name} fallback background differs')
            require(sum(abs(a - b) for a, b in zip(first, last)) / len(first) < 0.4,
                    f'{name} loop endpoint jumps')


def layout_cases(pack):
    layout = json.loads((pack / 'pack.json').read_text())['layout']
    stage = layout['stage']
    css = SCREEN_CSS.read_text()
    page = re.search(r':root\s*\{[^}]*background:\s*(#[\da-f]{6})', css)[1]
    require(stage['background'] == page, 'stage background differs from the page')
    area = layout['safe_area']
    width, height = 9.6 * area['width'], 6 * area['height']
    w, h = width * stage['scale'] / 2, height * stage['scale'] / 2
    cx, cy = (stage['anchor']['x'] * width / 100, stage['anchor']['y'] * height / 100)
    region = (cx - w / 2, cy - h / 2, w, h)

    def slot(name, w, h):
        anchor = layout['slots'][name]
        x, y = anchor['x'] * width / 100, anchor['y'] * height / 100
        return min(x, width - w), min(y, height - h), w, h

    # Reserve space for the clock/date and for each mutually exclusive prompt.
    status = slot('status', 208, 42)
    groups = [[region, status, slot('clock', 120, 44), slot('date', 120, 28)]]
    groups.extend([region, status, slot(state, 540, 174)] for state in ('link_code', 'approval', 'error'))
    for group in groups:
        for i, (x, y, w, h) in enumerate(group):
            require(x >= 0 and y >= 0 and x + w <= width and y + h <= height, 'layout leaves viewport')
            for a, b, m, n in group[i + 1:]:
                require(min(x + w, a + m) <= max(x, a) or min(y + h, b + n) <= max(y, b),
                        'layout slots overlap')
    require(region[2] > width / 2 and region[3] > height / 2, 'stage does not span most of viewport')
    print(f'FACE 960x600: stage={page} extent={region[2]:g}x{region[3]:g} overlap_area=0')


def eyes_cases(pack, decode):
    rest = decode(pack / 'rest.png', 'null')
    require(len(rest) == SIZE[0] * SIZE[1] * 4 and rest[3] == 0, 'rest size or alpha differs')
    require(not any(rest[SIZE[0] * 270 * 4 + 3::4]), 'rest has mouth or label pixels below eyes')
    print('FACE rest: label_pixels=0')
    idle_and_focus(pack, decode)
    muted_and_speaking(pack, decode)
    clip_boundaries(pack, decode)
    layout_cases(pack)


def contact_sheet(pack, destination, decode):
    width, height = 240, 150
    columns, rows = 4, 3
    stride = width * columns * 3
    background = bytes((16, 24, 39))
    sheet = bytearray(background * width * height * columns * rows)
    for i, (name, frame) in enumerate(SAMPLES.items()):
        extension = 'png' if name == 'rest' else 'mp4'
        raw = decode(pack / f'{name}.{extension}', f'select=eq(n\\,{frame}),scale={width}:{height}')
        require(len(raw) == width * height * 4, f'no contact frame for {name}')
        for y in range(height):
            for x in range(width):
                src = (y * width + x) * 4
                dst = (i // columns * height + y) * stride + (i % columns * width + x) * 3
                alpha = raw[src + 3]
                sheet[dst:dst + 3] = bytes((a * alpha + b * (255 - alpha)) // 255
                                           for a, b in zip(raw[src:src + 3], background))
    subprocess.run(['ffmpeg', '-v', 'error', '-f', 'rawvideo', '-pixel_format', 'rgb24',
                    '-video_size', f'{width * columns}x{height * rows}', '-i', 'pipe:0',
                    '-frames:v', '1', '-threads', '1', '-update', '1', str(destination)],
                   input=sheet, check=True, timeout=10)
