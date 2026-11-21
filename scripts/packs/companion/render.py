"""Rasterize the selected art and render silent, neutral-ended clips."""

import math
from pathlib import Path
import re
import subprocess

ART = Path(__file__).with_name("art")
WIDTH, HEIGHT, FPS = 768, 720, 30
SIZE = f"{WIDTH}x{HEIGHT}"
BACKGROUND = "#101827"
FRAME_BYTES = WIDTH * HEIGHT * 4
FFMPEG = ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
          "-filter_threads", "1", "-filter_complex_threads", "1"]
# The bust is a little taller than the frame, so it runs from just under the
# top edge off the bottom edge, and the idle bob never shows the cut waist.
POSE_HEIGHT = 736
POSE_TOP = 10
CENTER = WIDTH // 2
# The idle bob moves in quarter pixels, rendered by supersampling.
SUBPIXEL = 4
BOB = 10

# Every frame is a whole generated image of one size and framing: the neutral
# master and one edit of it per mouth shape, blink and state. They share one
# rectangle (x, y, w, h), so they register with each other; the renderer only
# places, grades, sequences and cross-fades them.
MASTER = (0, 6, 1024, 1530)
VARIANTS = ("mouth-half", "mouth-open", "blink", "listening", "connecting",
            "thinking", "error", "happy", "setup", "approval")
# An edit redraws the whole picture, so its hair, clothes and outlines sit a
# little off the master's. While she speaks the mouth shape changes every few
# frames, and that drift would flicker over her whole body. A mouth frame is
# therefore the master with its edit shown only over a region: a feather
# radius and shapes in master pixels (the 1024 x 1536 art), ("ellipse",
# centre x, centre y, radius x, radius y) or ("rect", left, top, right,
# bottom). The edit shows fully inside a shape and fades into the master over
# the feather around it; every other pixel is the master's own. The state
# poses and the blink are whole edits: their motion is meant.
MOUTH = (16, (("ellipse", 527, 523, 82, 44),))
REGIONS = {f"mouth-{shape}": MOUTH for shape in ("half", "open", "a", "i", "u", "e", "o")}
# Poses shown next to a card stand left of centre, clear of the card.
CARD_SHIFT = {"setup": -110, "approval": -110, "error": -110}
# A generated edit comes back a few levels brighter than its master, by an
# amount that differs with the colour. Each edit is therefore matched to the
# master with one colour map for the whole frame, fitted on a MATCH_SIZE copy
# over the pixels the edit left alone: those that differ by less than
# MATCH_SAME. The map is a lattice of MATCH_NODES values per channel.
MATCH_SIZE = (256, 384)
MATCH_SAME = 24
MATCH_NODES = 9
# One colour grade for every frame, from -1 (cool) to 1 (warm): red gains and
# blue loses a fifth of it. The build's --warmth option overrides it.
WARMTH = -0.25

# Unequal held mouth shapes, from the first frame after the neutral one. The
# runs never repeat as a shorter regular speech cycle.
SPEECH = (("mouth-half", 2), ("mouth-open", 4), ("mouth-half", 2), ("neutral", 2),
          ("mouth-half", 2), ("mouth-open", 3), ("mouth-half", 3),
          ("mouth-open", 4), ("mouth-half", 2), ("neutral", 3),
          ("mouth-half", 3), ("mouth-open", 5), ("mouth-half", 2), ("neutral", 2),
          ("mouth-open", 3), ("mouth-half", 2), ("mouth-open", 4), ("mouth-half", 3),
          ("neutral", 4),
          ("mouth-half", 2), ("mouth-open", 4), ("mouth-half", 2), ("neutral", 2),
          ("mouth-half", 3), ("mouth-open", 3), ("mouth-half", 2),
          ("mouth-open", 5), ("mouth-half", 3), ("neutral", 3),
          ("mouth-half", 2), ("mouth-open", 4), ("mouth-half", 4), ("neutral", 4))
# The idle loop is on screen most of the time, and every frame the display is
# given costs it a decode and a full redraw. The loop is therefore one slow
# breath, drawn IDLE_FPS times a second, and then a hold: she is still but for
# her blinks, and the clip carries no frame while nothing changes. The blinks
# come at unequal gaps: 6.9 s, 4.6 s and, across the loop's seam, 4.5 s. A
# blink is two frames: closed, then opening.
BREATH = 120
IDLE_LENGTH = 480
BLINKS = (42, 249, 387)
IDLE_FPS = 10
IDLE_STEP = FPS // IDLE_FPS
# A state loop returns to the neutral frame only where it joins, so the held
# poses use long loops.
FRAMES = {"idle": IDLE_LENGTH // IDLE_STEP, "connecting": 150, "listening": 240, "thinking": 240,
          "speaking": 100, "muted": 30, "setup": 60, "setup-enter": 16, "approval": 300,
          "error": 300, "happy": 45}
TRANSITION = 10
# These clips are the neutral frame with a bob or a mouth, or a pose held
# left of centre beside a card. A cross-fade between the centred neutral frame
# and a shifted pose would show two of her, so the page glides between them.
UNBLENDED = ("idle", "speaking", "muted", *CARD_SHIFT)
# Whole generated frames differ a little everywhere, which lossless coding
# cannot keep small. These clips are coded at a visually lossless quality.
LOSSY = ("idle", "speaking")


MATCHES = None
SMALL = {}
MASKS = {}


def run(args, data=None):
    return subprocess.run([*FFMPEG, *args], input=data, check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def raw_input(path, rate=FPS):
    return ["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", SIZE,
            "-framerate", str(rate), "-i", str(path)]


def small(sheet):
    width, height = MATCH_SIZE
    return run(["-i", str(ART / f"{sheet}.png"), "-vf",
                f"format=rgba,scale={width}:{height}:flags=area", "-frames:v", "1",
                "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])


def colour_match(sheet):
    """The colour map that takes an edit's colours to the master's, as a
    .cube file: every node holds the mean shift of the unchanged pixels near
    it, shared out by their distance to the node."""
    path = MATCHES / f"{sheet}.cube"
    if path.exists():
        return path
    if "neutral" not in SMALL:
        SMALL["neutral"] = small("neutral")
    master, edit = SMALL["neutral"], small(sheet)
    nodes = MATCH_NODES
    step = 255 / (nodes - 1)
    weight = [0.0] * nodes ** 3
    shift = [[0.0] * nodes ** 3 for _ in range(3)]
    count = 0
    mean = [0.0, 0.0, 0.0]
    for at in range(0, len(edit), 4):
        if edit[at + 3] < 250 or master[at + 3] < 250:
            continue
        colour = edit[at:at + 3]
        change = [master[at + c] - colour[c] for c in range(3)]
        if max(map(abs, change)) >= MATCH_SAME:
            continue
        count += 1
        cell, part = [], []
        for c in range(3):
            position = min(colour[c] / step, nodes - 1.000001)
            cell.append(int(position))
            part.append(position - int(position))
            mean[c] += change[c]
        for corner in range(8):
            node, share = 0, 1.0
            for c in range(3):
                high = corner >> c & 1
                node += (cell[c] + high) * nodes ** c
                share *= part[c] if high else 1 - part[c]
            weight[node] += share
            for c in range(3):
                shift[c][node] += share * change[c]
    assert count > 10000, f"{sheet} does not register with the master"
    mean = [total / count for total in mean]
    lines = [f"LUT_3D_SIZE {nodes}"]
    # Red changes fastest in a .cube file.
    for node in range(nodes ** 3):
        values = []
        for c in range(3):
            # A colour too rare to measure takes the frame's mean shift.
            known = weight[node] >= 20
            moved = shift[c][node] / weight[node] if known else mean[c]
            level = node // nodes ** c % nodes * step + moved
            values.append(f"{min(1.0, max(0.0, level / 255)):.6f}")
        lines.append(" ".join(values))
    path.write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")
    return path


def decoded(sheet, rect):
    """The sheet's RGBA pixels inside rect; an edit's colours are matched to
    the master's."""
    x, y, width, height = rect
    match = "" if sheet == "neutral" else f",lut3d=file={colour_match(sheet)}"
    return run(["-i", str(ART / f"{sheet}.png"), "-vf",
                f"format=rgba,crop={width}:{height}:{x}:{y}{match}", "-frames:v", "1",
                "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])


def distance(shape, x, y):
    """Pixels from (x, y) out to the shape; 0 inside it."""
    kind, *values = shape
    if kind == "rect":
        left, top, right, bottom = values
        return math.hypot(max(left - x, 0, x - right), max(top - y, 0, y - bottom))
    centre_x, centre_y, radius_x, radius_y = values
    u, v = (x - centre_x) / radius_x, (y - centre_y) / radius_y
    level = math.hypot(u, v)
    if level <= 1:
        return 0.0
    # Near the edge, the ellipse's level above 1 over the level's slope; never
    # nearer than the ellipse's own box, so the weight is 0 outside bounds().
    return max((level - 1) * level / math.hypot(u / radius_x, v / radius_y),
               abs(x - centre_x) - radius_x, abs(y - centre_y) - radius_y)


def bounds(region):
    """The box (left, top, right, bottom), in master pixels, outside which a
    region shows none of its edit."""
    feather, shapes = region
    boxes = [shape[1:] if shape[0] == "rect" else
             (shape[1] - shape[3], shape[2] - shape[4], shape[1] + shape[3], shape[2] + shape[4])
             for shape in shapes]
    return (min(box[0] for box in boxes) - feather, min(box[1] for box in boxes) - feather,
            max(box[2] for box in boxes) + feather, max(box[3] for box in boxes) + feather)


def weight(region, x, y):
    """How much of the edit shows at master pixel (x, y), from 0 to 255: all
    of it inside a shape, then a smooth step down to none over the feather."""
    feather, shapes = region
    outside = min(distance(shape, x, y) for shape in shapes) / feather
    if outside >= 1:
        return 0
    return round(255 * (1 - outside * outside * (3 - 2 * outside)))


def mask(region, rect):
    """The region's weight at every pixel of rect, one bytes row per row;
    None for a row it leaves to the master."""
    key = (region, rect)
    if key not in MASKS:
        x, y, width, height = rect
        left, top, right, bottom = bounds(region)
        left, right = max(0, math.floor(left) - x), min(width, math.ceil(right) - x + 1)
        top, bottom = max(0, math.floor(top) - y), min(height, math.ceil(bottom) - y + 1)
        rows = [None] * height
        for row in range(top, bottom):
            rows[row] = (bytes(left) + bytes(weight(region, x + column, y + row)
                                             for column in range(left, right))
                         + bytes(width - right))
        MASKS[key] = rows
    return MASKS[key]


def merge(master, edit, rows, width):
    """The master with the edit shown by each pixel's weight. Colours are
    mixed premultiplied, so a moved silhouette leaves no dark fringe."""
    out = bytearray(master)
    stride = width * 4
    for row, weights in enumerate(rows):
        if weights is None:
            continue
        at = row * stride
        for run_of in re.finditer(rb"\xff+|[\x01-\xfe]+", weights):
            start, stop = run_of.span()
            if weights[start] == 255:
                out[at + start * 4:at + stop * 4] = edit[at + start * 4:at + stop * 4]
                continue
            for column in range(start, stop):
                share = weights[column]
                i = at + column * 4
                old, new = master[i + 3] * (255 - share), edit[i + 3] * share
                cover = old + new
                out[i + 3] = (cover + 127) // 255
                for c in range(3):
                    out[i + c] = ((master[i + c] * old + edit[i + c] * new + cover // 2) // cover
                                  if cover else 0)
    return bytes(out)


def source(sheet, rect):
    """The file of a frame's unplaced pixels: the edit's own, or the master's
    with the edit shown only inside the sheet's region."""
    path = MATCHES / f"{sheet}@{'.'.join(map(str, rect))}.rgba"
    if not path.exists():
        if sheet not in REGIONS:
            pixels = decoded(sheet, rect)
        else:
            pixels = merge(source("neutral", rect).read_bytes(), decoded(sheet, rect),
                           mask(REGIONS[sheet], rect), rect[2])
        path.write_bytes(pixels)
    return path


def cutout(sheet, rect, filters):
    width, height = rect[2:]
    grade = f"colorchannelmixer=rr={1 + WARMTH / 5:.4f}:bb={1 - WARMTH / 5:.4f},"
    return run(["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", f"{width}x{height}",
                "-i", str(source(sheet, rect)), "-vf",
                f"format=rgba,{grade if WARMTH else ''}"
                f"lutrgb=a='if(lt(val,16),0,val)',{filters}", "-frames:v", "1",
                "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])


def pose_width(rect):
    return round(rect[2] * POSE_HEIGHT / rect[3])


def raster(sheet, rect, drop=0):
    """Place a pose in the frame, `drop` quarter pixels below POSE_TOP."""
    width = pose_width(rect)
    x = CENTER - width // 2
    top = POSE_TOP * SUBPIXEL + drop
    # Lanczos resamples the atlas cutout at four times the frame size. The
    # area filter then averages it down, which gives the sub-pixel position.
    return cutout(sheet, rect, (
        f"scale={width * SUBPIXEL}:{POSE_HEIGHT * SUBPIXEL}:flags=lanczos,"
        f"pad={WIDTH * SUBPIXEL}:{POSE_HEIGHT * SUBPIXEL + top}:{x * SUBPIXEL}:{top}"
        f":color=black@0,crop={WIDTH * SUBPIXEL}:{HEIGHT * SUBPIXEL}:0:0,"
        f"scale={WIDTH}:{HEIGHT}:flags=area"))


def mix(first, second):
    """The halfway frame of a cross-fade between two registered frames."""
    return bytes(map(lambda a, b: (a + b + 1) >> 1, first, second))


def keyframes():
    frames = {name: raster(name, MASTER) for name in VARIANTS}
    for name, x in CARD_SHIFT.items():
        frames[name] = shift(frames[name], x, 0)
    frames["bob"] = [raster("neutral", MASTER, drop) for drop in range(BOB * SUBPIXEL + 1)]
    frames["neutral"] = frames["bob"][0]
    return frames


def shift(frame, x, y):
    if x == 0 and y == 0:
        return frame
    # The registered poses leave room for these small translations.
    stride = WIDTH * 4
    start = max(0, -y) * stride
    stop = FRAME_BYTES - max(y, 0) * stride
    rows = [bytes(stride * max(y, 0))]
    for row in range(start, stop, stride):
        pixels = frame[row + 4 * max(-x, 0):row + stride - 4 * max(x, 0)]
        rows.append(bytes(4 * max(x, 0)) + pixels + bytes(4 * max(-x, 0)))
    rows.append(bytes(stride * max(-y, 0)))
    return b"".join(rows)


def bob(index):
    """Quarter pixels below the neutral frame: one slow breath per BREATH."""
    return round(BOB * SUBPIXEL * (1 - math.cos(math.tau * index / BREATH)) / 2)


def idle(frames):
    for index in range(0, IDLE_LENGTH, IDLE_STEP):
        drop = bob(index) if index < BREATH else 0
        frame = frames["bob"][drop]
        step = min(((index - start) // IDLE_STEP for start in BLINKS
                    if 0 <= index - start < 2 * IDLE_STEP), default=None)
        if step is not None:
            # The closed eyes ride the same breath as the frame they replace.
            closed = raster("blink", MASTER, drop)
            frame = mix(frame, closed) if step else closed
        yield frame


def speaking(frames):
    # The body is the neutral frame; only the mouth changes. Each new mouth
    # shape is entered through the halfway frame of a cross-fade.
    shapes = [pose for pose, run_length in SPEECH for _ in range(run_length)]
    assert len(shapes) == FRAMES["speaking"] - 2
    shapes = ["neutral", *shapes, "neutral"]
    mixed = {}
    for index, shape in enumerate(shapes):
        last = shapes[index - 1] if index else shape
        if last == shape:
            yield frames[shape]
            continue
        pair = tuple(sorted((last, shape)))
        if pair not in mixed:
            mixed[pair] = mix(frames[pair[0]], frames[pair[1]])
        yield mixed[pair]


def timeline(role, frames):
    if role == "idle":
        yield from idle(frames)
        return
    if role == "speaking":
        yield from speaking(frames)
        return
    length = FRAMES[role]
    for index in range(length):
        phase = index / (length - 1)
        if role == "muted":
            yield frames["neutral"]
        elif role == "happy":
            yield shift(frames[role], 0, -round(8 * math.sin(math.pi * phase) ** 2))
        elif role == "setup-enter":
            yield frames["setup"]
        else:
            yield frames[role]


def ease_in_out(frame, count):
    amount = f"max(0,min(1,min({frame},{count}-1-({frame}))/{TRANSITION}))"
    return f"(1-cos(PI*({amount})))/2"


def encode(pack, raw, role, count, keep=None):
    # Blend premultiplied colors so the short transitions have clean edges
    # on the alpha path. Both outputs derive from the exact same frames.
    # The blend is worked out for every pixel, which is most of the build's
    # time. It therefore runs only on the frames of a transition; every other
    # frame is the pose itself.
    if role in UNBLENDED:
        transition, blended = "1", "0"
    elif role == "setup-enter":
        # Ends on the card pose, which the setup loop then holds.
        transition, blended = f"(1-cos(PI*(N-1)/{count - 1}))/2", "1"
    else:
        transition = ease_in_out("N-1", count)
        blended = f"lte(n,{TRANSITION})+gte(n,{count - TRANSITION})"
    rate = IDLE_FPS if role == "idle" else FPS
    # Only the frames in keep are coded, each at its own time.
    held = "" if keep is None else "select='" + "+".join(f"eq(n,{n})" for n in keep) + "',"
    timing = [] if keep is None else ["-fps_mode", "vfr"]
    count = count if keep is None else len(keep)
    graph = (
        "[0:v]format=gbrap,premultiply=inplace=1[rest];"
        "[1:v]format=gbrap,premultiply=inplace=1[pose];"
        f"[pose][rest]blend=all_expr='B+(A-B)*({transition})':shortest=1"
        f":enable='{blended}',"
        f"unpremultiply=inplace=1,format=yuva420p,{held}split[webm][alpha];"
        f"color=c={BACKGROUND}:s={SIZE}:r={rate}[background];"
        "[background][alpha]overlay=shortest=1,format=yuv420p[mp4]"
    )
    fixed = ["-an", "-threads", "1", "-fflags", "+bitexact",
             "-flags:v", "+bitexact", "-map_metadata", "-1", "-frames:v", str(count)]
    # Encode the held frame identically, even across the first keyframe.
    gop = ["-g", "1"] if role == "muted" else []
    h264_quality = ["-qp", "18"] if role == "muted" else ["-crf", "18"]
    vp9_quality = (["-crf", "10", "-b:v", "0", "-cpu-used", "2"] if role in LOSSY
                   else ["-lossless", "1", "-cpu-used", "4"])
    run(["-loop", "1", "-framerate", str(rate), "-i", str(pack / "rest.png"),
         *raw_input(raw, rate), "-filter_complex", graph,
         "-map", "[webm]", *fixed, *timing, *gop, "-c:v", "libvpx-vp9",
         "-auto-alt-ref", "0", *vp9_quality, str(pack / f"{role}.webm"),
         "-map", "[mp4]", *fixed, *timing, *gop, "-c:v", "libx264", *h264_quality, "-preset", "fast",
         "-x264-params", "aq-mode=0:mbtree=0", "-movflags", "+faststart",
         str(pack / f"{role}.mp4")])


def render(pack, work, warmth=WARMTH):
    global WARMTH, MATCHES
    WARMTH = warmth
    MATCHES = work
    SMALL.clear()
    frames = keyframes()
    run([*raw_input("pipe:0"), "-frames:v", "1", "-threads", "1",
         str(pack / "rest.png")], frames["neutral"])
    raw = work / "frames.rgba"
    for role, count in FRAMES.items():
        keep, last = [], None
        with raw.open("wb") as stream:
            for index, frame in enumerate(timeline(role, frames)):
                # The last frame is kept so the clip keeps its length.
                if frame != last or index == count - 1:
                    keep.append(index)
                last = frame
                stream.write(frame)
        encode(pack, raw, role, count, keep if role == "idle" else None)
