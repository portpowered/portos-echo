"""Decoded-art checks for the generated default face (one frame px per CSS px)."""
import json

from test_echo_install import require

SIZE = (960, 600)
MOUTH_ROWS = range(420, 500)


def frame_at(pack, name, number, decode):
    raw = decode(pack / f'{name}.webm', f'select=eq(n\\,{number})', True)
    require(len(raw) == SIZE[0] * SIZE[1] * 4, f'no frame {number} for {name}')
    return raw


def bright(frame, x, y):
    i = (y * SIZE[0] + x) * 4
    return frame[i + 3] >= 128 and max(frame[i:i + 3]) >= 128


def boxes(frame, rows=range(0, 400)):
    """Bounding boxes of the bright core of the left and right eye."""
    found = [[SIZE[0], SIZE[1], -1, -1], [SIZE[0], SIZE[1], -1, -1]]
    for y in rows:
        for x in range(0, SIZE[0], 2):
            if bright(frame, x, y):
                box = found[x >= SIZE[0] // 2]
                box[:] = [min(box[0], x), min(box[1], y), max(box[2], x), max(box[3], y)]
    return [(x1 - x0 + 2, y1 - y0 + 1, x0, y0) for x0, y0, x1, y1 in found]


def mouth(frame):
    return any(bright(frame, x, y) for y in MOUTH_ROWS for x in range(400, 560, 4))


def face_cases(pack, decode):
    manifest = json.loads((pack / 'pack.json').read_text())
    layout = manifest['layout']
    require(layout['stage'] == {'anchor': {'x': 50, 'y': 50}, 'scale': 2,
                                'background': '#101827'}, 'face stage is not full page')
    slots = layout['slots']
    require(slots['clock']['x'] == slots['date']['x'] == 50, 'time is not centred')
    require((pack / 'pack.css').read_text() == '.pack-clock, .pack-date { text-align: center; }\n',
            'pack.css differs')
    rest = decode(pack / 'rest.png', 'null')
    require(len(rest) == SIZE[0] * SIZE[1] * 4, 'rest size differs')
    idle = frame_at(pack, 'idle', 0, decode)
    for width, height, _, _ in boxes(idle):
        require(width >= 220 and abs(width / height - 1) <= 0.05, f'eye {width}x{height} is not round')
    left, right = boxes(idle)
    span = (right[2] + right[0] - left[2]) / SIZE[0]
    require(0.6 <= span <= 0.7, f'eye pair spans {span:.2f}')
    # No ghost: with the gaze far left, the rest eye footprints are clear.
    glance = frame_at(pack, 'idle', 120, decode)
    for x in (380, 760):
        require(glance[(240 * SIZE[0] + x) * 4 + 3] < 16, 'a rest footprint covers the frame')
    require(not mouth(idle), 'idle has a mouth')
    for name in ('wake', 'listening', 'thinking', 'speaking'):
        require(mouth(frame_at(pack, name, 0, decode)), f'{name} has no mouth')
    awake, talking = (frame_at(pack, name, 0, decode) for name in ('wake', 'speaking'))
    eye_rows = SIZE[0] * 400 * 4
    require(awake[:eye_rows] == talking[:eye_rows], 'wake and speaking eyes differ')
    print(f'FACE default: eyes={left[0]}x{left[1]} span={span:.2f} ghost=0 mouth=session')
