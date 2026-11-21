"""T1: a held clip looks as the 30 fps clip it replaces did."""

import itertools
import operator
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_companion_screen_pack as screen_pack  # noqa: E402
from test_companion_screen_pack import render  # noqa: E402

# A held clip's H.264 frames are coded apart from those of the 30 fps clip,
# so the two differ by codec noise: mean RGB levels. Measured at most 0.56:
# the 30 fps clip's P-frames drift from its own first frame while the pose
# holds, and the held clip goes on showing that first frame. The VP9 clips
# are lossless and must match exactly.
HOLD_NOISE = {"webm": 0, "mp4": 1.0}
# Every frame is compared exactly; the mean difference of two H.264 frames is
# worked out where the held clip codes a frame and at every SAMPLE-th frame.
SAMPLE = 30
# A held clip codes its two cross-fades, the pose after each, and its second
# and last frames: at most 23 frames in 2 to 10 s, where the 30 fps clip coded
# 30 to 300.
HOLD_FRAMES = 2 * render.TRANSITION + 3
# Clips whose loop was shorter as a 30 fps clip; the held clip is compared
# with that clip looped.
PREVIOUS = {"muted": 30}


def opaque_delta(a, b):
    """screen_pack.delta() of two frames whose alpha channels are equal, as decoded H.264
    frames' are."""
    return sum(map(abs, map(operator.sub, a, b))) / (len(a) * .75)


def frames_of(path, extension, rate=30):
    """Each frame of a clip decoded at rate, one at a time."""
    command = screen_pack.decoder(path, extension, rate)
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
        while frame := process.stdout.read(screen_pack.FRAME_BYTES):
            assert len(frame) == screen_pack.FRAME_BYTES, path
            yield frame
    assert process.returncode == 0, path


def coded(path):
    """The frames a clip carries."""
    return int(subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets",
         "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0", str(path)],
        capture_output=True, check=True, text=True).stdout)


class CompanionHolds(unittest.TestCase):
    """T1: a held clip looks as the 30 fps clip it replaces did. Each held
    clip is encoded both ways from the same frames; decoded at 30 fps, every
    frame of the held clip is the 30 fps clip's frame of that time. Needs
    ffmpeg only; no pack is built."""

    @classmethod
    def setUpClass(cls):
        directory = tempfile.TemporaryDirectory(prefix="companion-holds-")
        cls.addClassCleanup(directory.cleanup)
        cls.root = Path(directory.name)
        render.MATCHES = cls.root
        frames = render.keyframes([role for role in render.HELD if role in render.VARIANTS],
                                  breath=False)
        cls.packs = {"held": cls.root / "held", "continuous": cls.root / "continuous"}
        for kind, pack in cls.packs.items():
            pack.mkdir()
            render.run([*render.raw_input("pipe:0"), "-frames:v", "1", "-threads", "1",
                        str(pack / "rest.png")], frames["neutral"])
            lengths = {} if kind == "held" else PREVIOUS
            with mock.patch.dict(render.FRAMES, lengths):
                for role in render.HELD:
                    render.clip(pack, cls.root, role, frames, hold=kind == "held")

    def previous(self, role, ext):
        """The 30 fps clip's frames, looped to the held clip's length."""
        path = self.packs["continuous"] / f"{role}.{ext}"
        length = PREVIOUS.get(role, render.FRAMES[role])
        self.assertEqual(coded(path), length)
        if role not in PREVIOUS:
            return frames_of(path, ext)
        loop = list(frames_of(path, ext))
        self.assertEqual(len(loop), length)
        return (loop[index % length] for index in range(render.FRAMES[role]))

    def test_held_clips_change_only_timing(self):
        for role, ext in itertools.product(render.HELD, ("webm", "mp4")):
            with self.subTest(role=role, codec=ext):
                held = self.packs["held"] / f"{role}.{ext}"
                count, worst, differing, measured, last = 0, 0.0, 0, 0, None
                for new, old in itertools.zip_longest(frames_of(held, ext),
                                                      self.previous(role, ext)):
                    self.assertIsNotNone(new, "the held clip is shorter")
                    self.assertIsNotNone(old, "the held clip is longer")
                    index, count = count, count + 1
                    fresh, last = new != last, new
                    if new == old:
                        continue
                    differing += 1
                    self.assertEqual(ext, "mp4", f"frame {index} differs")
                    if fresh or index % SAMPLE == 0:
                        measured += 1
                        worst = max(worst, opaque_delta(new, old))
                self.assertEqual(count, render.FRAMES[role])
                self.assertLessEqual(worst, HOLD_NOISE[ext], measured)
                frames = coded(held)
                self.assertLessEqual(frames, HOLD_FRAMES)
                was = PREVIOUS.get(role, render.FRAMES[role])
                print(f"HOLD role={role} codec={ext} frames={count} coded={frames} "
                      f"was={was} differing={differing} measured={measured} "
                      f"worst_mean={worst:.3f} tolerance={HOLD_NOISE[ext]}", flush=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
