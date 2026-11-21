"""T2: exercise the offline CLI and decode the actual distributable media."""

import hashlib
import importlib.util
import io
import itertools
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/packs/companion"))
import render  # noqa: E402

ROLES = ("idle", "connecting", "listening", "thinking", "speaking", "muted",
         "setup", "approval", "error", "happy")
ARCHIVE = "companion-0.2.2.screenpack.tar.gz"
SUCCESS = "screenpack: companion=0.2.2 generated=PASS checksum=PASS\n"
FAILURE = "screenpack: generation failed; output was not published\n"
WIDTH, HEIGHT = 768, 720
FRAME_BYTES = WIDTH * HEIGHT * 4
BACKGROUND = (16, 24, 39)
HOME_CENTER, CARD_CENTER = 384, 274
# The poses that stand beside a card for as long as it shows.
CARD = ("setup", "approval", "error")
# The idle loop is timed at 10 fps; its first blink is frame 14.
RATES = {"idle": 10}
BLINK = 14
# Two states are told apart when their middle frames differ by this much.
DISTINCT = 0.4


def invoke(out, *, env=None):
    return subprocess.run(["sh", "scripts/make-companion-screenpack.sh",
                           "--version", "0.2.2", "--out", str(out)],
                          cwd=ROOT, capture_output=True, text=True, env=env)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def decoder(path, extension, rate):
    """ffmpeg arguments that decode a clip to RGBA frames at rate: a frame
    the clip holds is repeated for as long as it holds."""
    codec = ["-c:v", "libvpx-vp9"] if extension == "webm" else []
    return ["ffmpeg", "-nostdin", "-hide_banner", "-threads", "1", *codec,
            "-i", str(path), "-map", "0", "-threads", "1", "-pix_fmt", "rgba",
            "-fps_mode", "cfr", "-r", str(rate), "-f", "rawvideo", "pipe:1"]


def decode(path, extension, rate=30):
    result = subprocess.run(decoder(path, extension, rate), capture_output=True, check=True)
    header = result.stderr.decode().split("Stream mapping:")[0]
    codec = "vp9" if extension == "webm" else "h264"
    assert len(re.findall(r"Stream #0:", header)) == 1, header
    assert f"Video: {codec}" in header, header
    assert re.search(r"\b768x720\b", header), header
    # An MP4 lists its mean frame rate, which a hold lowers; a WebM its own.
    if extension == "webm":
        assert re.search(rf"\b{rate} fps\b", header), header
    assert len(result.stdout) % FRAME_BYTES == 0, header
    if extension == "webm":
        assert re.search(r"alpha_mode\s*:\s*1", header), header
    frames = memoryview(result.stdout)
    return [frames[i:i + FRAME_BYTES] for i in range(0, len(frames), FRAME_BYTES)]


def crop(frame, region, down=0):
    left, top, right, bottom = region
    return b"".join(frame[((y + down) * WIDTH + left) * 4:((y + down) * WIDTH + right) * 4]
                    for y in range(top, bottom))


def changed(a, b):
    """The box of the pixels that differ between two registered frames."""
    xs, ys = [], []
    for y in range(HEIGHT):
        row = y * WIDTH * 4
        hits = [x for x in range(WIDTH)
                if abs(a[row + x * 4 + 1] - b[row + x * 4 + 1]) > 48]
        if hits:
            ys.append(y)
            xs += [hits[0], hits[-1]]
    assert ys, "the frames do not differ"
    return min(xs), min(ys), max(xs) + 1, max(ys) + 1


def delta(a, b):
    assert len(a) == len(b) and a and b
    return sum(abs(x - y) for i, (x, y) in enumerate(zip(a, b)) if i % 4 != 3) / (len(a) * .75)


def visual(frame, has_alpha):
    if not has_alpha:
        return frame
    out = bytearray(frame)
    for i in range(0, len(frame), 4):
        alpha = frame[i + 3]
        if alpha < 255:
            for c, background in enumerate(BACKGROUND):
                out[i + c] = (frame[i + c] * alpha + background * (255 - alpha)) // 255
    return out


def classify(frame, region, references, drops=(0,)):
    return min(references, key=lambda key: min(
        delta(crop(frame, region, drop), references[key]) for drop in drops))


def visible_center(frame, with_alpha):
    # Sample the silhouette every four pixels. The fallback's matte is
    # uniform, so use red-channel contrast to ignore minor codec variation.
    threshold = bytes(int(value >= 16 if with_alpha else abs(value - BACKGROUND[0]) >= 24)
                      for value in range(256))
    left, right = WIDTH, -1
    channel = 3 if with_alpha else 0
    for start in range(0, FRAME_BYTES, WIDTH * 4 * 4):
        row = bytes(frame[start + channel:start + WIDTH * 4:16]).translate(threshold)
        first = row.find(b"\x01")
        if first != -1:
            left = min(left, first * 4)
            right = max(right, row.rfind(b"\x01") * 4)
    assert left < right, "decoded silhouette is empty"
    return (left + right) / 2


class CompanionPack(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        directory = tempfile.TemporaryDirectory(prefix="companion-contract-")
        cls.addClassCleanup(directory.cleanup)
        cls.root = Path(directory.name)
        cls.first = cls.root / "first"
        result = invoke(cls.first)
        if (result.returncode, result.stdout, result.stderr) != (0, SUCCESS, ""):
            raise AssertionError((result.returncode, result.stdout, result.stderr))
        cls.archive = cls.first / ARCHIVE
        cls.extracted = cls.root / "extracted"
        cls.extracted.mkdir()
        with tarfile.open(cls.archive, "r:gz") as bundle:
            bundle.extractall(cls.extracted, filter="data")

    def test_repeatable_bounded_download(self):
        second = self.root / "second"
        result = invoke(second)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, SUCCESS, ""))
        checksum = digest(self.archive)
        self.assertEqual(digest(second / ARCHIVE), checksum)
        for folder in (self.first, second):
            self.assertEqual((folder / (ARCHIVE + ".sha256")).read_bytes(),
                             f"{checksum}  {ARCHIVE}\n".encode())
        shapes = ("half", "open", "a", "i", "u", "e", "o")
        expected = {"pack.json", "rest.png", "companion-clock.css",
                    *(f"mouth-{shape}.png" for shape in shapes),
                    *(f"{role}.{ext}" for role in (*ROLES, "setup-enter") for ext in ("webm", "mp4"))}
        with tarfile.open(self.archive) as bundle:
            members = bundle.getmembers()
            self.assertEqual(sorted(m.name for m in members), sorted(expected))
            self.assertLessEqual(sum(m.size + 1024 for m in members), 64 * 1024 ** 2)
            self.assertLessEqual(self.archive.stat().st_size, 64 * 1024 ** 2)
            for member in members:
                self.assertTrue(member.isfile(), member.name)
                self.assertEqual(member.mode, 0o644)
                if member.name.endswith((".mp4", ".webm")):
                    self.assertLessEqual(member.size, 8 * 1024 ** 2)
                self.assertEqual((self.extracted / member.name).read_bytes(),
                                 (self.first / "companion@0.2.2" / member.name).read_bytes())
        manifest = json.loads((self.extracted / "pack.json").read_bytes())
        self.assertEqual((manifest["format"], manifest["name"], manifest["version"]),
                         (1, "companion", "0.2.2"))
        character = manifest["character"]
        self.assertEqual(character["rest"], "rest.png")
        states = {
            role: {"clip": {"mode": "loop", "webm": f"{role}.webm", "mp4": f"{role}.mp4"}}
            for role in ROLES if role != "happy"}
        states["setup"]["enter"] = {
            "mode": "once", "webm": "setup-enter.webm", "mp4": "setup-enter.mp4"}
        self.assertEqual(character["states"], states)
        self.assertEqual(character["emotes"], {
            "happy": {"mode": "once", "webm": "happy.webm", "mp4": "happy.mp4"}})
        # The lip-sync stills: the neutral frame is the closed mouth, and each
        # still is a whole frame of the clips' size.
        self.assertEqual(character["mouth"], {
            "closed": "rest.png", **{shape: f"mouth-{shape}.png" for shape in shapes}})
        rest = (self.extracted / "rest.png").read_bytes()
        for shape in shapes:
            still = (self.extracted / f"mouth-{shape}.png").read_bytes()
            self.assertEqual(still[:24], rest[:24], shape)
            self.assertNotEqual(still, rest, shape)
        print(f"PACK identity=companion@0.2.2 files={len(expected)} sha256={checksum} "
              f"archive_bytes={self.archive.stat().st_size} repeat=identical", flush=True)

    def test_release_allows_only_approved_stylesheet(self):
        spec = importlib.util.spec_from_file_location("package_dist", ROOT / "scripts/package-dist.py")
        package = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(package)
        layout = json.loads((self.extracted / "pack.json").read_bytes())["layout"]
        stylesheet = "companion-clock.css"
        self.assertEqual(layout["css"], stylesheet)
        files = package.screen_pack_payload(self.archive, "companion", "0.2.2")
        self.assertEqual(files[stylesheet], (self.extracted / stylesheet).read_bytes())

        altered = self.root / "extra-css" / ARCHIVE
        altered.parent.mkdir()
        with tarfile.open(self.archive) as source, tarfile.open(altered, "w:gz") as target:
            for member in source:
                target.addfile(member, source.extractfile(member))
            extra = tarfile.TarInfo("other.css")
            extra.size = len(files[stylesheet])
            target.addfile(extra, io.BytesIO(files[stylesheet]))
        with self.assertRaisesRegex(package.BundleError, "^invalid companion members$"):
            package.screen_pack_payload(altered, "companion", "0.2.2")
        print("RELEASE-CSS companion-clock.css=ACCEPTED other.css=REFUSED", flush=True)

    def test_occupied_and_failed_encoder(self):
        sentinel = self.first / "keep"
        sentinel.write_bytes(b"keep these bytes")
        before = digest(self.archive)
        result = invoke(self.first)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (1, "", FAILURE))
        self.assertEqual(sentinel.read_bytes(), b"keep these bytes")
        self.assertEqual(digest(self.archive), before)
        tools = self.root / "tools"
        tools.mkdir()
        encoder = tools / "ffmpeg"
        encoder.write_text(
            '#!/bin/sh\nfor argument do\n  if [ "$argument" = libx264 ]; then\n'
            '    printf "%s\\n" reached > "$FAULT_MARKER"\n    exit 73\n  fi\ndone\n'
            'exec "$REAL_FFMPEG" "$@"\n', encoding="ascii")
        encoder.chmod(0o755)
        marker = self.root / "encoder-called"
        out = self.root / "absent"
        result = invoke(out, env={**os.environ, "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
                                  "REAL_FFMPEG": shutil.which("ffmpeg"), "FAULT_MARKER": str(marker)})
        self.assertEqual((result.returncode, result.stdout, result.stderr), (1, "", FAILURE))
        self.assertEqual(marker.read_text(encoding="ascii"), "reached\n")
        self.assertFalse(out.exists())
        self.assertEqual(list(self.root.glob(".companion-*")), [])
        print("REFUSAL occupied=preserved encoder_exit=73 output=absent staging=absent", flush=True)

    def test_media_in_both_decoders(self):
        render.MATCHES = self.root
        art = render.keyframes()
        eyes_at = changed(art["neutral"], art["blink"])
        mouth_at = changed(art["neutral"], art["mouth-open"])
        eyes = {key: crop(art[key], eyes_at) for key in ("neutral", "blink")}
        mouths = {key: crop(art[key], mouth_at)
                  for key in ("neutral", "mouth-half", "mouth-open")}
        for ext in ("webm", "mp4"):
            with self.subTest(codec=ext):
                self.check_media(ext, art["neutral"], (eyes_at, eyes), (mouth_at, mouths))

    def check_media(self, ext, rest, eyes, mouths):
        with_alpha = ext == "webm"
        rest = visual(rest, True)
        representatives = {}
        for role in ROLES:
            frames = decode(self.extracted / f"{role}.{ext}", ext, RATES.get(role, 30))
            # Each clip lasts as long in both codecs, its holds included.
            self.assertEqual(len(frames), render.FRAMES[role], (ext, role, "length"))
            first, last = (visual(frame, with_alpha) for frame in (frames[0], frames[-1]))
            self.assertLess(delta(first, last), 1.5, (ext, role, "join"))
            centers = [visible_center(frame, with_alpha) for frame in (frames[0], frames[-1])]
            if role in CARD:
                # A card pose is held from its first frame: nothing of the
                # centred neutral frame shows through it.
                for center in centers:
                    self.assertAlmostEqual(center, CARD_CENTER, delta=32, msg=(ext, role))
            else:
                self.assertLess(delta(first, rest), 4, (ext, role, "start"))
                self.assertLess(delta(last, rest), 4, (ext, role, "end"))
            # The idle loop is told apart by its breath, which is deepest
            # two seconds in; the rest of the loop is a hold.
            middle = 2 * RATES["idle"] - 2 if role == "idle" else len(frames) // 2
            representatives[role] = visual(frames[middle], with_alpha)
            if role == "setup":
                self.check_setup(ext, rest, frames, first)
            if role == "idle":
                # She breathes, so the eyes sit up to BOB pixels lower.
                drops = range(render.BOB + 1)
                blink = [classify(frames[BLINK + step], *eyes, drops) for step in (-2, -1, 0, 2, 3)]
                self.assertEqual(blink, ["neutral", "neutral", "blink", "neutral", "neutral"])
            if role == "speaking":
                speech = [classify(frame, *mouths) for frame in frames[12:-12]]
                self.assertEqual(set(speech), {"neutral", "mouth-half", "mouth-open"})
                self.assertFalse(any(speech[p:] == speech[:-p] for p in range(1, len(speech) // 2)))
                self.assertGreater(sum(a != b for a, b in itertools.pairwise(speech)), 6)
                # Outside the mouth the decoded frames differ by codec noise only.
                changes = [index + 12 for index, (a, b) in
                           enumerate(itertools.pairwise(speech)) if a != b][:8]
                body = max(outside_delta(visual(frames[index], with_alpha),
                                         visual(frames[0], with_alpha),
                                         [frame_box(render.MOUTH)]) for index in changes)
                self.assertLessEqual(body, CODEC_NOISE[ext], ext)
                print(f"SPEAKING codec={ext} outside_mouth_mean={body:.3f} "
                      f"tolerance={CODEC_NOISE[ext]}", flush=True)
            if role == "muted":
                self.assertEqual(len({hashlib.sha256(frame).digest() for frame in frames}), 1)
            if with_alpha:
                self.assertLess(frames[0][3], 16)
                center = CARD_CENTER if role in CARD else HOME_CENTER
                self.assertGreater(frames[0][(229 * WIDTH + center) * 4 + 3], 240)
        pairs = tuple(itertools.combinations(representatives, 2))
        # Speaking is the neutral frame but for its mouth, so it is told apart
        # by the mouth alone.
        mouth_at = mouths[0]
        differences = [
            delta(crop(representatives[a], mouth_at), crop(representatives[b], mouth_at))
            if "speaking" in (a, b) else delta(representatives[a], representatives[b])
            for a, b in pairs]
        same = [(pair, value) for pair, value in zip(pairs, differences) if value <= DISTINCT]
        self.assertEqual(same, [])
        print(f"MEDIA codec={ext} distinct_pairs={len(pairs)}/{len(pairs)} "
              f"least_difference={min(differences):.3f} blink_frame={BLINK} "
              f"mouth_classes={len(set(speech))} shorter_period=none muted_unique=1", flush=True)

    def check_setup(self, ext, rest, held, held_first):
        with_alpha = ext == "webm"
        entry = decode(self.extracted / f"setup-enter.{ext}", ext)
        first, last = (visual(frame, with_alpha) for frame in (entry[0], entry[-1]))
        self.assertLess(delta(first, rest), 4, (ext, "setup-enter", "start"))
        seam = delta(last, held_first)
        self.assertLess(seam, 1.5, (ext, "setup-enter to setup"))
        entry_centers = [visible_center(frame, with_alpha) for frame in entry]
        held_centers = [visible_center(frame, with_alpha) for frame in held]
        self.assertAlmostEqual(entry_centers[0], HOME_CENTER, delta=12)
        self.assertAlmostEqual(entry_centers[-1], CARD_CENTER, delta=32)
        self.assertTrue(all(right <= left + 4 for left, right in itertools.pairwise(entry_centers)),
                        entry_centers)
        self.assertTrue(all(abs(center - CARD_CENTER) <= 32 for center in held_centers),
                        held_centers)
        print(f"SETUP codec={ext} entry={entry_centers[0]}->{entry_centers[-1]} "
              f"held_frames={len(held)} held_center={min(held_centers)}..{max(held_centers)} "
              f"entry_join_delta={seam:.3f}", flush=True)


def frame_box(region):
    """A region's bounds in frame pixels, grown by the reach of the resampling
    that places the art: (left, top, right, bottom), clipped to the frame."""
    x, y, width, height = render.MASTER
    scale_x, scale_y = render.pose_width(render.MASTER) / width, render.POSE_HEIGHT / height
    origin = render.CENTER - render.pose_width(render.MASTER) // 2
    left, top, right, bottom = render.bounds(region)
    return (max(0, int(origin + (left - x) * scale_x) - REACH),
            max(0, int(render.POSE_TOP + (top - y) * scale_y) - REACH),
            min(WIDTH, int(origin + (right - x) * scale_x) + 1 + REACH),
            min(HEIGHT, int(render.POSE_TOP + (bottom - y) * scale_y) + 1 + REACH))


def outside_delta(a, b, boxes):
    """Mean absolute RGBA difference over the pixels outside every box."""
    total = count = 0
    for y in range(HEIGHT):
        row = y * WIDTH * 4
        cuts = sorted((left, right) for left, top, right, bottom in boxes if top <= y < bottom)
        spans, x = [], 0
        for left, right in cuts:
            if left > x:
                spans.append((x, left))
            x = max(x, right)
        spans.append((x, WIDTH))
        for start, stop in spans:
            if stop <= start:
                continue
            first = a[row + start * 4:row + stop * 4]
            second = b[row + start * 4:row + stop * 4]
            count += len(first)
            if first != second:
                total += sum(abs(p - q) for p, q in zip(first, second))
    return total / count


# Lanczos at four times the frame size reaches three art pixels past a
# region; in frame pixels that is under two, and the area filter adds one.
REACH = 4
# Placed frames are compared before any codec, so outside their regions they
# must equal the master exactly.
RAW_TOLERANCE = 0
# The speaking clip is lossy in both codecs, so two of its frames may differ a
# little even where their pixels were equal: mean RGBA levels outside the
# mouth. Measured 0.000 (VP9) and 0.208 (H.264); before the mouth frames were
# composited the placed frames alone differed there by 0.90 to 1.32.
CODEC_NOISE = {"webm": 0.1, "mp4": 0.5}
# The seam a region's feather leaves: at most this many levels between two
# neighbouring pixels comes from the change of weight alone. A hard-edged
# region would step by the whole edit-to-master difference, up to 255.
SEAM_STEP = 12


class CompanionMasks(unittest.TestCase):
    """T1: a mouth frame differs from the master only inside the mouth
    region, so nothing but her mouth moves while she speaks. Needs ffmpeg
    only; no pack is built."""

    @classmethod
    def setUpClass(cls):
        directory = tempfile.TemporaryDirectory(prefix="companion-masks-")
        cls.addClassCleanup(directory.cleanup)
        render.MATCHES = Path(directory.name)
        cls.frames = {sheet: render.raster(sheet, render.MASTER)
                      for sheet in ("neutral", *render.REGIONS)}

    def test_every_mouth_has_a_region(self):
        mouths = {path.stem for path in render.ART.glob("mouth-*.png")}
        self.assertEqual(mouths, set(render.REGIONS))
        # Every frame the speaking clip plays is the master or a mouth.
        self.assertLessEqual({shape for shape, _ in render.SPEECH} - {"neutral"},
                             set(render.REGIONS))

    def test_outside_its_region_a_frame_is_the_master(self):
        master = self.frames["neutral"]
        for sheet, region in render.REGIONS.items():
            box = frame_box(region)
            outside = outside_delta(self.frames[sheet], master, [box])
            inside = delta(crop(self.frames[sheet], box), crop(master, box))
            self.assertLessEqual(outside, RAW_TOLERANCE, sheet)
            self.assertGreater(inside, 0.5, sheet)
            print(f"MASK sheet={sheet} box={box} outside_mean={outside:.3f} "
                  f"inside_mean={inside:.2f}", flush=True)

    def test_two_mouths_agree_outside_both_regions(self):
        worst = 0
        for first, second in itertools.combinations(render.REGIONS, 2):
            boxes = [frame_box(render.REGIONS[first]), frame_box(render.REGIONS[second])]
            difference = outside_delta(self.frames[first], self.frames[second], boxes)
            self.assertLessEqual(difference, RAW_TOLERANCE, (first, second))
            worst = max(worst, difference)
        pairs = len(render.REGIONS) * (len(render.REGIONS) - 1) // 2
        print(f"MASK pairs={pairs} worst_outside_mean={worst:.3f} "
              f"tolerance={RAW_TOLERANCE}", flush=True)

    def test_seam_is_smooth(self):
        rect = render.MASTER
        width = rect[2]
        master = render.source("neutral", rect).read_bytes()
        for sheet, region in render.REGIONS.items():
            edit = render.decoded(sheet, rect)
            rows = render.mask(region, rect)
            # Weights run from 255 inside a shape to 0 outside its feather,
            # with no jump at the box the weights are worked out in.
            self.assertEqual({rows[0], rows[-1]} - {None}, set(), sheet)
            seam = hard = 0
            for y, weights in enumerate(rows):
                if weights is None:
                    continue
                self.assertEqual((weights[0], weights[-1]), (0, 0), (sheet, y))
                below = rows[y + 1] if y + 1 < len(rows) else None
                for x in range(width):
                    share = weights[x]
                    for other in (weights[x + 1] if x + 1 < width else share,
                                  below[x] if below is not None else 0):
                        if other == share:
                            continue
                        i = (y * width + x) * 4
                        change = max(abs(edit[i + c] - master[i + c]) for c in range(3))
                        seam = max(seam, abs(share - other) * change / 255)
                        hard = max(hard, change)
            self.assertLessEqual(seam, SEAM_STEP, sheet)
            print(f"SEAM sheet={sheet} feather={region[0]} step={seam:.1f} "
                  f"hard_edge_step={hard} budget={SEAM_STEP}", flush=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
