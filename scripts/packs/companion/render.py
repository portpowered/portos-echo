"""Rasterize the selected art and render silent, neutral-ended clips."""

import math
from pathlib import Path
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

# Artwork rectangles (x, y, w, h), with atlas gutters removed. Every pose is
# scaled to POSE_HEIGHT, so each rectangle is a bust of the same proportion.
# The neutral frame and its variants are whole generated images of one size
# and framing. They share one rectangle, so they register with each other;
# the renderer only places, sequences and cross-fades them.
MASTER = (0, 6, 1024, 1530)
VARIANTS = ("mouth-half", "mouth-open", "blink", "listening")
POSES = {
    "connecting": ("poses", (634, 12, 287, 487)),
    "thinking": ("poses", (113, 524, 309, 487)),
    "error": ("poses", (613, 523, 320, 488)),
    "happy": ("poses", (1159, 524, 264, 487)),
    "setup": ("cards", (103, 18, 610, 847)),
    "approval": ("cards", (1068, 16, 525, 851)),
}
# Poses shown next to a card stand left of centre, clear of the card.
CARD_SHIFT = {"setup": -110, "approval": -110, "error": -110}

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
# The idle loop is three breaths long, so the blinks in it come at unequal
# gaps: 3.7 s, 3.2 s and, across the loop's seam, 5.1 s. A blink is four
# frames: closing, closed twice, opening.
BREATH = 120
BLINKS = (42, 153, 249)
# A state loop returns to the neutral frame only where it joins, so the held
# poses use long loops.
FRAMES = {"idle": 3 * BREATH + 1, "connecting": 150, "listening": 240, "thinking": 240,
          "speaking": 100, "muted": 30, "setup": 60, "setup-enter": 16, "approval": 300,
          "error": 300, "happy": 45}
TRANSITION = 10
# These clips are the neutral frame with a bob or a mouth, or a held card
# pose: there is nothing to blend.
UNBLENDED = ("idle", "speaking", "muted", "setup")
# Whole generated frames differ a little everywhere, which lossless coding
# cannot keep small. These clips are coded at a visually lossless quality.
LOSSY = ("idle", "speaking")


def run(args, data=None):
    return subprocess.run([*FFMPEG, *args], input=data, check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def raw_input(path):
    return ["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", SIZE,
            "-framerate", str(FPS), "-i", str(path)]


def cutout(sheet, rect, filters):
    x, y, width, height = rect
    return run(["-i", str(ART / f"{sheet}.png"), "-vf",
                f"format=rgba,crop={width}:{height}:{x}:{y},"
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
    frames = {name: raster(sheet, rect) for name, (sheet, rect) in POSES.items()}
    for name, x in CARD_SHIFT.items():
        frames[name] = shift(frames[name], x, 0)
    for name in VARIANTS:
        frames[name] = raster(name, MASTER)
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
    for index in range(FRAMES["idle"]):
        drop = bob(index)
        frame = frames["bob"][drop]
        step = min((index - start for start in BLINKS if 0 <= index - start < 4), default=None)
        if step is not None:
            # The closed eyes ride the same breath as the frame they replace.
            closed = raster("blink", MASTER, drop)
            frame = closed if step in (1, 2) else mix(frame, closed)
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


def encode(pack, raw, role, count):
    # Blend premultiplied colors so the short transitions have clean edges
    # on the alpha path. Both outputs derive from the exact same frames.
    if role in UNBLENDED:
        transition = "1"
    elif role == "setup-enter":
        # Ends on the card pose, which the setup loop then holds.
        transition = f"(1-cos(PI*(N-1)/{count - 1}))/2"
    else:
        transition = ease_in_out("N-1", count)
    graph = (
        "[0:v]format=gbrap,premultiply=inplace=1[rest];"
        "[1:v]format=gbrap,premultiply=inplace=1[pose];"
        f"[rest][pose]blend=all_expr='A+(B-A)*({transition})':shortest=1,"
        "unpremultiply=inplace=1,format=yuva420p,split[webm][alpha];"
        f"color=c={BACKGROUND}:s={SIZE}:r={FPS}[background];"
        "[background][alpha]overlay=shortest=1,format=yuv420p[mp4]"
    )
    fixed = ["-an", "-threads", "1", "-fflags", "+bitexact",
             "-flags:v", "+bitexact", "-map_metadata", "-1", "-frames:v", str(count)]
    # Encode the held frame identically, even across the first keyframe.
    gop = ["-g", "1"] if role == "muted" else []
    h264_quality = ["-qp", "18"] if role == "muted" else ["-crf", "18"]
    vp9_quality = (["-crf", "10", "-b:v", "0", "-cpu-used", "2"] if role in LOSSY
                   else ["-lossless", "1", "-cpu-used", "4"])
    run(["-loop", "1", "-framerate", str(FPS), "-i", str(pack / "rest.png"),
         *raw_input(raw), "-filter_complex", graph,
         "-map", "[webm]", *fixed, *gop, "-c:v", "libvpx-vp9",
         "-auto-alt-ref", "0", *vp9_quality, str(pack / f"{role}.webm"),
         "-map", "[mp4]", *fixed, *gop, "-c:v", "libx264", *h264_quality, "-preset", "fast",
         "-x264-params", "aq-mode=0:mbtree=0", "-movflags", "+faststart",
         str(pack / f"{role}.mp4")])


def render(pack, work):
    frames = keyframes()
    run([*raw_input("pipe:0"), "-frames:v", "1", "-threads", "1",
         str(pack / "rest.png")], frames["neutral"])
    raw = work / "frames.rgba"
    for role, count in FRAMES.items():
        with raw.open("wb") as stream:
            stream.writelines(timeline(role, frames))
        encode(pack, raw, role, count)
