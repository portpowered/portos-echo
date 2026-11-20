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
POSE_HEIGHT = 680
POSE_TOP = 24
POSE_SCALE = POSE_HEIGHT / 292
# Inside the fixed stage, these centers map to the middle of the home
# screen and the middle of the left two thirds at 1280 × 748.
HOME_CENTER = 506
CARD_CENTER = 252

# Artwork rectangles (x, y, w, h), with atlas gutters removed. Keep the head
# crown and waist aligned when changing poses.
POSES = {
    "rest": ("reference", (166, 6, 564, 881)),
    "idle": ("poses", (97, 11, 327, 491)),
    "connecting": ("poses", (634, 12, 287, 490)),
    "listening": ("listening", (191, 150, 614, 1227)),
    "thinking": ("poses", (113, 524, 309, 490)),
    "error": ("poses", (613, 523, 320, 491)),
    "happy": ("poses", (1159, 524, 264, 490)),
    "setup": ("cards", (103, 18, 610, 852)),
    "approval": ("cards", (1068, 16, 525, 856)),
}

# The lip/eyelid patches are feathered inside the face. Their placements are
# in the original 256 × 320 registered raster, scaled with the idle sprite.
PATCHES = {
    "blink": ((718, 300, 252, 72), (96, 67, 66, 22)),
    "half": ((1329, 394, 110, 80), (111, 94, 29, 23)),
    "open": ((1869, 396, 110, 80), (111, 94, 29, 23)),
}

# Unequal held mouth shapes within one loop. The whole clip restarts at the
# rest pose. The runs never repeat as a shorter regular speech cycle.
SPEECH = (("idle", 9), ("half", 5), ("open", 8), ("half", 4), ("idle", 10),
          ("open", 5), ("half", 8), ("idle", 4), ("half", 4), ("open", 10),
          ("idle", 12))
FRAMES = {"idle": 126, "connecting": 72, "listening": 48, "thinking": 72,
          "speaking": 100, "muted": 30, "setup": 60, "setup-enter": 16, "approval": 60,
          "error": 60, "happy": 45}
TRANSITION = 10


def run(args, data=None):
    return subprocess.run([*FFMPEG, *args], input=data, check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def raw_input(path):
    return ["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", SIZE,
            "-framerate", str(FPS), "-i", str(path)]


def raster(sheet, rect, output_size, pad=""):
    x, y, width, height = rect
    out_width, out_height = output_size
    # Lanczos resamples the existing atlas cutouts directly at the new size.
    filters = (f"format=rgba,crop={width}:{height}:{x}:{y},"
               f"lutrgb=a='if(lt(val,16),0,val)',"
               f"scale={out_width}:{out_height}:flags=lanczos{pad}")
    return run(["-i", str(ART / f"{sheet}.png"), "-vf", filters, "-frames:v", "1",
                "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])


def blend_patch(frame, pixels, x, y, width, height):
    pose = bytearray(frame)
    for row in range(height):
        for column in range(width):
            i = (row * width + column) * 4
            j = ((y + row) * WIDTH + x + column) * 4
            edge = min(row, height - 1 - row, column, width - 1 - column)
            alpha = min(1, edge / (4 * POSE_SCALE)) * (pixels[i + 3] / 255)
            for channel in range(3):
                pose[j + channel] = round(
                    pose[j + channel] * (1 - alpha) + pixels[i + channel] * alpha)
    return bytes(pose)


def face_patch(idle, source, placement):
    x, y, width, height = placement
    x = HOME_CENTER + round((x - 128) * POSE_SCALE)
    y = POSE_TOP + round((y - 12) * POSE_SCALE)
    width, height = round(width * POSE_SCALE), round(height * POSE_SCALE)
    pixels = raster("expression", source, (width, height))
    return blend_patch(idle, pixels, x, y, width, height)


def listening_mouth(listening, idle):
    # Register the original closed lips with the new pose. The feathered
    # margin blends the surrounding skin without changing the lip shape.
    x, y, width, height = 463, 218, 76, 57
    pixels = bytearray(b"".join(
        idle[(row * WIDTH + x) * 4:(row * WIDTH + x + width) * 4]
        for row in range(y, y + height)))
    # Fade toward the cheek and nose instead of imposing a rectangle of
    # Ready's face shading on the new pose.
    for row in range(height):
        for column in range(width):
            radius = math.hypot((2 * column + 1) / width - 1, (2 * row + 1) / height - 1)
            opacity = min(1, max(0, (1 - radius) / 0.56))
            alpha = (row * width + column) * 4 + 3
            pixels[alpha] = round(pixels[alpha] * opacity ** 2 * (3 - 2 * opacity))
    return blend_patch(listening, pixels, x + 32, y - 6, width, height)


def keyframes():
    frames = {}
    for name, (sheet, rect) in POSES.items():
        width = round(rect[2] * POSE_HEIGHT / rect[3])
        x = HOME_CENTER - width // 2
        frames[name] = raster(sheet, rect, (width, POSE_HEIGHT),
                              f",pad={WIDTH}:{HEIGHT}:{x}:{POSE_TOP}:color=black@0")
    for name, (source, placement) in PATCHES.items():
        frames[name] = face_patch(frames["idle"], source, placement)
    frames["listening"] = listening_mouth(frames["listening"], frames["idle"])
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
    for index in range(length):
        if role in ("muted", "setup-enter") or index in (0, length - 1):
            yield frames["rest"]
            continue
        cycle = math.tau * index / (length - 1)
        name, x, y = role, 0, 0
        if role == "idle":
            name = ("blink" if 62 <= index < 66 else
                    "connecting" if 88 <= index < 98 else role)
            # Hold the image registration across the four-frame blink.
            y = -1 if 56 <= index <= 70 else round(math.cos(cycle) - 1)
        elif role == "speaking":
            name = speech[min(max(0, index - TRANSITION), len(speech) - 1)]
            x = round(2 * math.sin(cycle))
        elif role == "happy":
            y = -round(9 * math.sin(cycle / 2) ** 2)
        elif role in ("connecting", "listening", "thinking"):
            x = round(2 * math.sin(cycle))
            y = -round(3 * math.sin(cycle / 2) ** 2)
        yield shift(frames[name], round(x * POSE_SCALE), round(y * POSE_SCALE))


def ease_in_out(frame, count):
    amount = f"max(0,min(1,min({frame},{count}-1-({frame}))/{TRANSITION}))"
    return f"(1-cos(PI*({amount})))/2"


def card_pan(role, count):
    if role == "setup-enter":
        amount = f"(1-cos(PI*n/{count - 1}))/2"
    elif role == "setup":
        amount = "1"
    elif role in ("approval", "error"):
        amount = ease_in_out("n", count)
    else:
        return ""
    # Move the neutral pose once on entry and hold card registration on replay.
    # Padding keeps the area revealed by the translation clear.
    shift = HOME_CENTER - CARD_CENTER
    return (f",pad={WIDTH + shift}:{HEIGHT}:0:0:color=black@0,"
            f"crop={SIZE.replace('x', ':')}:x='{shift}*({amount})':y=0:exact=1")


def encode(pack, raw, role, count):
    # Blend premultiplied colors so the short transitions have clean edges
    # on the alpha path. Both outputs derive from the exact same frames.
    transition = ease_in_out("N-1", count)
    graph = (
        "[0:v]format=gbrap,premultiply=inplace=1[rest];"
        "[1:v]format=gbrap,premultiply=inplace=1[pose];"
        f"[rest][pose]blend=all_expr='A+(B-A)*({transition})':shortest=1,"
        f"unpremultiply=inplace=1{card_pan(role, count)},format=yuva420p,split[webm][alpha];"
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
         "-auto-alt-ref", "0", "-lossless", "1", str(pack / f"{role}.webm"),
         "-map", "[mp4]", *fixed, *gop, "-c:v", "libx264", *h264_quality, "-preset", "fast",
         "-x264-params", "aq-mode=0:mbtree=0", "-movflags", "+faststart",
         str(pack / f"{role}.mp4")])


def render(pack, work):
    frames = keyframes()
    run([*raw_input("pipe:0"), "-frames:v", "1", "-threads", "1",
         str(pack / "rest.png")], frames["rest"])
    raw = work / "frames.rgba"
    for role, count in FRAMES.items():
        with raw.open("wb") as stream:
            stream.writelines(timeline(role, frames))
        encode(pack, raw, role, count)
