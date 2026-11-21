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
NEUTRAL = ("poses", (97, 11, 327, 488))
POSES = {
    "connecting": ("poses", (634, 12, 287, 487)),
    "listening": ("listening", (191, 150, 614, 1045)),
    "thinking": ("poses", (113, 524, 309, 487)),
    "error": ("poses", (613, 523, 320, 488)),
    "happy": ("poses", (1159, 524, 264, 487)),
    "setup": ("cards", (103, 18, 610, 847)),
    "approval": ("cards", (1068, 16, 525, 851)),
}

# Mouth shapes from the expression sheet. Each head on that sheet was
# registered to the neutral pose by correlating its eyes and nose (not its
# mouth) with the pose at a third of a pixel: the same 0.4304 x 0.4310 scale
# fits every head, and the second pair is where the sheet's origin lands in
# poses.png. The rectangle is the mouth, from under the nostrils to the chin.
EXPRESSION_SCALE = (0.4304, 0.4310)
MOUTHS = {
    "half": ((1323, 406, 118, 70), (-331.373, -15.386)),
    "open": ((1865, 406, 118, 72), (-565.034, -15.354)),
}

# Unequal held mouth shapes, from the first frame after the neutral one. The
# runs never repeat as a shorter regular speech cycle.
SPEECH = (("half", 2), ("open", 4), ("half", 2), ("neutral", 2),
          ("half", 2), ("open", 3), ("half", 3),
          ("open", 4), ("half", 2), ("neutral", 3),
          ("half", 3), ("open", 5), ("half", 2), ("neutral", 1),
          ("open", 3), ("half", 2), ("open", 4), ("half", 3), ("neutral", 4),
          ("half", 2), ("open", 4), ("half", 2), ("neutral", 2),
          ("half", 3), ("open", 3), ("half", 2),
          ("open", 5), ("half", 3), ("neutral", 3),
          ("half", 2), ("open", 4), ("half", 4), ("neutral", 5))
# A state loop returns to the neutral frame only where it joins, so the held
# poses use long loops.
FRAMES = {"idle": 121, "connecting": 150, "listening": 240, "thinking": 240,
          "speaking": 100, "muted": 30, "setup": 60, "setup-enter": 16, "approval": 300,
          "error": 300, "happy": 45}
TRANSITION = 10
# These clips are the neutral frame with a bob or a mouth, or a held card
# pose: there is nothing to blend.
UNBLENDED = ("idle", "speaking", "muted", "setup")


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


def blend_patch(frame, pixels, x, y, width, height):
    pose = bytearray(frame)

    def offsets(row, column):
        return (row * width + column) * 4, ((y + row) * WIDTH + x + column) * 4

    # The two sheets shade the skin a little differently. Match the patch to
    # the face where they meet: its outer ring holds only skin in both.
    ring = [offsets(row, column) for row in range(height) for column in range(width)
            if min(row, height - 1 - row, column, width - 1 - column) < 3]
    tone = [sum(pose[j + channel] - pixels[i + channel] for i, j in ring) / len(ring)
            for channel in range(3)]
    for row in range(height):
        for column in range(width):
            i, j = offsets(row, column)
            # Fade the rectangle's border into the face around it.
            edge = (min(1, min(column, width - 1 - column) / 12)
                    * min(1, min(row, height - 1 - row) / 8))
            alpha = edge * edge * (3 - 2 * edge) * (pixels[i + 3] / 255)
            for channel in range(3):
                color = min(255, max(0, pixels[i + channel] + tone[channel]))
                pose[j + channel] = round(pose[j + channel] * (1 - alpha) + color * alpha)
    return bytes(pose)


def mouth_frame(neutral, source, origin):
    rect = NEUTRAL[1]
    scale = POSE_HEIGHT / rect[3]
    x, y, width, height = source
    # Expression sheet -> poses.png -> the frame.
    x = (CENTER - pose_width(rect) // 2
         + (origin[0] + x * EXPRESSION_SCALE[0] - rect[0]) * scale)
    y = POSE_TOP + (origin[1] + y * EXPRESSION_SCALE[1] - rect[1]) * scale
    size = (round(width * EXPRESSION_SCALE[0] * scale),
            round(height * EXPRESSION_SCALE[1] * scale))
    pixels = cutout("expression", source, f"scale={size[0]}:{size[1]}:flags=lanczos")
    return blend_patch(neutral, pixels, round(x), round(y), *size)


def keyframes():
    frames = {name: raster(sheet, rect) for name, (sheet, rect) in POSES.items()}
    frames["bob"] = [raster(*NEUTRAL, drop) for drop in range(BOB * SUBPIXEL + 1)]
    frames["neutral"] = frames["bob"][0]
    for name, (source, origin) in MOUTHS.items():
        frames[name] = mouth_frame(frames["neutral"], source, origin)
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


def timeline(role, frames):
    length = FRAMES[role]
    speech = tuple(pose for pose, run_length in SPEECH for _ in range(run_length))
    assert len(speech) == FRAMES["speaking"] - 2
    neutral = frames["neutral"]
    for index in range(length):
        phase = index / (length - 1)
        if role == "idle":
            # One slow breath: down from the neutral frame and back to it.
            yield frames["bob"][round(BOB * SUBPIXEL * (1 - math.cos(math.tau * phase)) / 2)]
        elif role == "speaking":
            # The body is the neutral frame; only the mouth changes.
            yield frames[speech[index - 1]] if 0 < index < length - 1 else neutral
        elif role == "muted":
            yield neutral
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
    run(["-loop", "1", "-framerate", str(FPS), "-i", str(pack / "rest.png"),
         *raw_input(raw), "-filter_complex", graph,
         "-map", "[webm]", *fixed, *gop, "-c:v", "libvpx-vp9",
         "-auto-alt-ref", "0", "-lossless", "1", "-cpu-used", "4", str(pack / f"{role}.webm"),
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
