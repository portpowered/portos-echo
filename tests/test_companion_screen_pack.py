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
from render import keyframes  # noqa: E402

ROLES = ("idle", "connecting", "listening", "thinking", "speaking", "muted",
         "setup", "approval", "error", "happy")
ARCHIVE = "companion-0.2.1.screenpack.tar.gz"
SUCCESS = "screenpack: companion=0.2.1 generated=PASS checksum=PASS\n"
FAILURE = "screenpack: generation failed; output was not published\n"
WIDTH, HEIGHT = 768, 720
FRAME_BYTES = WIDTH * HEIGHT * 4
EYES = (.5735, .2233, .7520, .2709)
MOUTH = (.6193, .3174, .6852, .3609)
BACKGROUND = (16, 24, 39)
HOME_CENTER, CARD_CENTER = 506, 252


def invoke(out, *, env=None):
    return subprocess.run(["sh", "scripts/make-companion-screenpack.sh",
                           "--version", "0.2.1", "--out", str(out)],
                          cwd=ROOT, capture_output=True, text=True, env=env)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def decode(path, extension):
    decoder = ["-c:v", "libvpx-vp9"] if extension == "webm" else []
    result = subprocess.run(
        ["ffmpeg", "-nostdin", "-hide_banner", "-threads", "1", *decoder,
         "-i", str(path), "-map", "0", "-threads", "1", "-pix_fmt", "rgba",
         "-f", "rawvideo", "pipe:1"], capture_output=True, check=True)
    header = result.stderr.decode().split("Stream mapping:")[0]
    codec = "vp9" if extension == "webm" else "h264"
    assert len(re.findall(r"Stream #0:", header)) == 1, header
    assert f"Video: {codec}" in header, header
    assert re.search(r"\b768x720\b", header), header
    assert re.search(r"\b30 fps\b", header), header
    assert len(result.stdout) % FRAME_BYTES == 0, header
    if extension == "webm":
        assert re.search(r"alpha_mode\s*:\s*1", header), header
    frames = memoryview(result.stdout)
    return [frames[i:i + FRAME_BYTES] for i in range(0, len(frames), FRAME_BYTES)]


def crop(frame, region, shift=0):
    left, top, right, bottom = (round(value * size)
                                for value, size in zip(region, (WIDTH, HEIGHT, WIDTH, HEIGHT)))
    return b"".join(frame[(y * WIDTH + left + shift) * 4:(y * WIDTH + right + shift) * 4]
                    for y in range(top, bottom))


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


def classify(frame, region, references, shifts):
    return min(references, key=lambda key: min(
        delta(crop(frame, region, shift), references[key]) for shift in shifts))


def card_reference(rest):
    offset = (HOME_CENTER - CARD_CENTER) * 4
    stride = WIDTH * 4
    matte = bytes((*BACKGROUND, 255)) * (HOME_CENTER - CARD_CENTER)
    return b"".join(rest[start + offset:start + stride] + matte
                    for start in range(0, len(rest), stride))


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
        expected = {"pack.json", "rest.png", "companion-clock.css",
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
                                 (self.first / "companion@0.2.1" / member.name).read_bytes())
        manifest = json.loads((self.extracted / "pack.json").read_bytes())
        self.assertEqual((manifest["format"], manifest["name"], manifest["version"]),
                         (1, "companion", "0.2.1"))
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
        print(f"PACK identity=companion@0.2.1 files={len(expected)} sha256={checksum} "
              f"archive_bytes={self.archive.stat().st_size} repeat=identical", flush=True)

    def test_release_allows_only_approved_stylesheet(self):
        spec = importlib.util.spec_from_file_location("package_dist", ROOT / "scripts/package-dist.py")
        package = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(package)
        layout = json.loads((self.extracted / "pack.json").read_bytes())["layout"]
        stylesheet = "companion-clock.css"
        self.assertEqual(layout["css"], stylesheet)
        files = package.screen_pack_payload(self.archive, "companion", "0.2.1")
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
            package.screen_pack_payload(altered, "companion", "0.2.1")
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
        art = keyframes()
        eyes = {key: crop(art[key], EYES) for key in ("idle", "blink")}
        mouths = {key: crop(art[key], MOUTH) for key in ("idle", "half", "open")}
        for ext in ("webm", "mp4"):
            with self.subTest(codec=ext):
                self.check_media(ext, art["rest"], eyes, mouths)

    def check_media(self, ext, rest, eyes, mouths):
        with_alpha = ext == "webm"
        rest = visual(rest, True)
        card_rest = card_reference(rest)
        representatives = {}
        for role in ROLES:
            frames = decode(self.extracted / f"{role}.{ext}", ext)
            first, last = (visual(frame, with_alpha) for frame in (frames[0], frames[-1]))
            neutral = card_rest if role == "setup" else rest
            self.assertLess(delta(first, neutral), 4, (ext, role, "start"))
            self.assertLess(delta(last, neutral), 4, (ext, role, "end"))
            self.assertLess(delta(first, last), 1.5, (ext, role, "join"))
            representatives[role] = visual(frames[len(frames) // 2], with_alpha)
            if role == "setup":
                self.check_setup(ext, rest, card_rest, frames, first)
            if role == "idle":
                blink = [classify(frame, EYES, eyes, (0,)) for frame in frames[57:70]]
                self.assertEqual(blink, ["idle"] * 5 + ["blink"] * 4 + ["idle"] * 4)
            if role == "speaking":
                speech = [classify(frame, MOUTH, mouths, range(-5, 6))
                          for frame in frames[12:-12]]
                self.assertEqual(set(speech), {"idle", "half", "open"})
                self.assertFalse(any(speech[p:] == speech[:-p] for p in range(1, len(speech) // 2)))
                self.assertGreater(sum(a != b for a, b in itertools.pairwise(speech)), 6)
            if role == "muted":
                self.assertEqual(len({hashlib.sha256(frame).digest() for frame in frames}), 1)
            if with_alpha:
                self.assertLess(frames[0][3], 16)
                center = CARD_CENTER if role == "setup" else HOME_CENTER
                self.assertGreater(frames[0][(229 * WIDTH + center) * 4 + 3], 240)
        pairs = tuple(itertools.combinations(representatives, 2))
        differences = [delta(representatives[a], representatives[b]) for a, b in pairs]
        self.assertEqual(sum(value > 1.5 for value in differences), 45,
                         [(pair, value) for pair, value in zip(pairs, differences) if value <= 1.5])
        print(f"MEDIA codec={ext} fps=30 distinct_pairs=45/45 neutral_endpoints=20/20 "
              f"blink_frames={blink.count('blink')} mouth_classes={len(set(speech))} "
              "shorter_period=none muted_unique=1", flush=True)

    def check_setup(self, ext, rest, card_rest, held, held_first):
        with_alpha = ext == "webm"
        entry = decode(self.extracted / f"setup-enter.{ext}", ext)
        first, last = (visual(frame, with_alpha) for frame in (entry[0], entry[-1]))
        self.assertLess(delta(first, rest), 4, (ext, "setup-enter", "start"))
        self.assertLess(delta(last, card_rest), 4, (ext, "setup-enter", "end"))
        seam = delta(last, held_first)
        self.assertLess(seam, 1.5, (ext, "setup-enter to setup"))
        entry_centers = [visible_center(frame, with_alpha) for frame in entry]
        held_centers = [visible_center(frame, with_alpha) for frame in held]
        self.assertAlmostEqual(entry_centers[0], HOME_CENTER, delta=12)
        self.assertAlmostEqual(entry_centers[-1], CARD_CENTER, delta=12)
        self.assertTrue(all(right <= left + 4 for left, right in itertools.pairwise(entry_centers)),
                        entry_centers)
        self.assertTrue(all(abs(center - CARD_CENTER) <= 32 for center in held_centers),
                        held_centers)
        print(f"SETUP codec={ext} entry={entry_centers[0]}->{entry_centers[-1]} "
              f"held_frames={len(held)} held_center={min(held_centers)}..{max(held_centers)} "
              f"entry_join_delta={seam:.3f}", flush=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
