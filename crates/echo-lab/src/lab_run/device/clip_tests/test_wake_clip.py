"""T1: direct calls to the shipped WAV parser, using only in-memory bytes."""
import hashlib
import importlib.util
from pathlib import Path
import struct
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "wake_clip", Path(__file__).resolve().parents[1] / "wake_clip.py")
CLIP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLIP)


def chunk(name, data):
    return name + struct.pack("<I", len(data)) + data + bytes(len(data) % 2)


def riff(*chunks):
    body = b"WAVE" + b"".join(chunks)
    return b"RIFF" + struct.pack("<I", len(body)) + body


def fmt(rate=16000, tag=1, channels=1, width=16, align=2, byte_rate=None):
    return struct.pack("<HHIIHH", tag, channels, rate,
                       rate * 2 if byte_rate is None else byte_rate, align, width)


def wav(rate=16000, samples=16000, format_bytes=None, extra=b""):
    return riff(chunk(b"fmt ", fmt(rate) if format_bytes is None else format_bytes),
                extra, chunk(b"data", bytes(samples * 2)))


class WakeClipParserTests(unittest.TestCase):
    def setUp(self):
        # Any accidental filesystem/process admission would violate this layer.
        for target in ("pathlib.Path.open", "subprocess.check_output"):
            guard = patch(target, side_effect=AssertionError("not an in-memory call"))
            guard.start()
            self.addCleanup(guard.stop)

    def reject(self, data, reason):
        with self.assertRaisesRegex(ValueError, "^" + reason + "$"):
            CLIP.validate(data)

    def test_both_rates_accept_inclusive_endpoints_and_one_second(self):
        for rate in (16000, 48000):
            for samples, duration in ((rate // 2, 500), (rate, 1000), (rate * 10, 10000)):
                with self.subTest(rate=rate, samples=samples):
                    data = wav(rate, samples)
                    self.assertEqual(CLIP.validate(data), {
                        "sample_rate_hz": rate, "duration_ms": duration,
                        "sha256": hashlib.sha256(data).hexdigest()})

    def test_duration_rejects_one_sample_outside_each_endpoint_and_empty(self):
        for rate in (16000, 48000):
            for samples in (0, rate // 2 - 1, rate * 10 + 1):
                with self.subTest(rate=rate, samples=samples):
                    self.reject(wav(rate, samples), "PCM format or duration")

    def test_header_signatures_length_and_size_bound(self):
        original = wav()
        for data in (b"", b"not WAV", original[:43], original[:-1],
                     original + b"\0", bytes(1000001),
                     b"RIFX" + original[4:], original[:8] + b"XXXX" + original[12:]):
            with self.subTest(length=len(data), prefix=data[:12]):
                self.reject(data, "RIFF")

    def test_format_tag_channel_width_rate_alignment_and_byte_rate(self):
        cases = ([{"tag": tag} for tag in (0, 3, 6, 65534)]
                 + [{"channels": count} for count in (0, 2)]
                 + [{"width": width} for width in (0, 8, 24, 32)]
                 + [{"rate": rate} for rate in (0, 8000, 44100)]
                 + [{"align": align} for align in (0, 1, 4)]
                 + [{"byte_rate": rate} for rate in (0, 16000, 32001)])
        for values in cases:
            with self.subTest(values=values):
                self.reject(wav(format_bytes=fmt(**values)), "PCM format or duration")

    def test_missing_duplicate_and_out_of_order_chunks(self):
        format_chunk = chunk(b"fmt ", fmt())
        data_chunk = chunk(b"data", bytes(32000))
        for chunks, reason in (
                ((data_chunk,), "data chunk"),
                ((format_chunk, chunk(b"JUNK", bytes(32))), "missing chunk"),
                ((chunk(b"JUNK", bytes(40)),), "missing chunk"),
                ((data_chunk, format_chunk), "data chunk"),
                ((format_chunk, format_chunk, data_chunk), "format chunk"),
                ((format_chunk, data_chunk, data_chunk), "data chunk")):
            with self.subTest(reason=reason, count=len(chunks)):
                self.reject(riff(*chunks), reason)

    def test_format_chunk_sizes_and_extensions(self):
        for size in (0, 15, 17, 19, 40):
            with self.subTest(size=size):
                self.reject(wav(format_bytes=(fmt() + bytes(40))[:size]), "format chunk")
        self.reject(wav(format_bytes=fmt() + b"\1\0"), "format extension")
        self.assertEqual(CLIP.validate(wav(format_bytes=fmt() + b"\0\0"))[
            "duration_ms"], 1000)

    def test_truncated_chunk_header_payload_and_padding(self):
        format_chunk = chunk(b"fmt ", fmt())
        data_chunk = chunk(b"data", bytes(32000))
        for tail in (b"x", b"data\0\0\0"):
            self.reject(riff(format_chunk, data_chunk, tail), "chunk header")
        for malformed in (b"data" + struct.pack("<I", 32002) + bytes(32000),
                          b"JUNK" + struct.pack("<I", 2) + b"x",
                          b"JUNK" + struct.pack("<I", 1) + b"x",
                          b"data" + struct.pack("<I", 0xffffffff)):
            self.reject(riff(format_chunk, data_chunk, malformed), "truncated chunk")

    def test_odd_data_length_with_present_padding_is_not_a_complete_sample(self):
        self.reject(riff(chunk(b"fmt ", fmt()), chunk(b"data", bytes(32001))),
                    "PCM format or duration")

    def test_ancillary_chunks_and_padding_do_not_change_duration(self):
        data = riff(chunk(b"JUNK", b"odd"), chunk(b"fmt ", fmt()),
                    chunk(b"LIST", b"ancillary"), chunk(b"data", bytes(32000)),
                    chunk(b"JUNK", bytes(7)))
        self.assertEqual(CLIP.validate(data)["duration_ms"], 1000)
        self.assertEqual(CLIP.validate(data)["sample_rate_hz"], 16000)

    def test_hash_covers_complete_wav_including_header_and_ancillary_bytes(self):
        original = wav()
        changed_header = wav(format_bytes=fmt() + b"\0\0")
        ancillary_a = wav(extra=chunk(b"JUNK", b"a"))
        ancillary_b = wav(extra=chunk(b"JUNK", b"b"))
        hashes = []
        for data in (original, changed_header, ancillary_a, ancillary_b):
            metadata = CLIP.validate(data)
            self.assertEqual(metadata["sha256"], hashlib.sha256(data).hexdigest())
            self.assertNotEqual(metadata["sha256"], hashlib.sha256(bytes(32000)).hexdigest())
            self.assertEqual(metadata["duration_ms"], 1000)
            hashes.append(metadata["sha256"])
        self.assertEqual(len(set(hashes)), 4)
        self.assertEqual(hashes[0],
                         "643f8a8dc8bd9c19225afffad2becfec5426180b3749cb208abdf1a6c8354efc")


if __name__ == "__main__":
    unittest.main()
