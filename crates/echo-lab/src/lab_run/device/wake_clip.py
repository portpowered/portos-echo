"""Offline, bounded WAV admission. Emit metadata and the exact immutable bytes."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys


def admit(path, repository):
    path = Path(path).resolve(strict=True)
    if not path.is_file():
        raise ValueError("not a regular file")
    roots = subprocess.check_output(
        ["git", "-C", repository, "worktree", "list", "--porcelain", "-z"])
    for field in roots.decode("utf-8").split("\0"):
        if field.startswith("worktree ") and path.is_relative_to(
                Path(field[9:]).resolve()):
            raise ValueError("repository input")
    with path.open("rb") as source:
        data = source.read(1000001)
    metadata = validate(data)
    return metadata, data


def validate(data):
    if (len(data) < 44 or len(data) > 1000000 or data[:4] != b"RIFF"
            or data[8:12] != b"WAVE"
            or struct.unpack_from("<I", data, 4)[0] != len(data) - 8):
        raise ValueError("RIFF")
    offset, fmt, samples = 12, None, None
    while offset < len(data):
        if offset + 8 > len(data):
            raise ValueError("chunk header")
        name, size = struct.unpack_from("<4sI", data, offset)
        start = offset + 8
        offset = start + size + size % 2
        if offset > len(data):
            raise ValueError("truncated chunk")
        if name == b"fmt ":
            if fmt is not None or size not in (16, 18):
                raise ValueError("format chunk")
            fmt = struct.unpack_from("<HHIIHH", data, start)
            if size == 18 and data[start + 16:start + 18] != b"\0\0":
                raise ValueError("format extension")
        if name == b"data":
            if samples is not None or fmt is None:
                raise ValueError("data chunk")
            samples = size
    if fmt is None or samples is None:
        raise ValueError("missing chunk")
    tag, channels, rate, byte_rate, align, width = fmt
    if (tag != 1 or channels != 1 or rate not in (16000, 48000)
            or width != 16 or align != 2 or byte_rate != rate * 2
            or samples % 2 or not rate <= samples <= rate * 20):
        raise ValueError("PCM format or duration")
    duration = samples * 500 / rate
    return {"sample_rate_hz": rate,
            "sha256": hashlib.sha256(data).hexdigest(),
            "duration_ms": int(duration) if duration.is_integer() else duration}


if __name__ == "__main__":
    try:
        metadata, data = admit(sys.argv[1], sys.argv[2])
        sys.stdout.buffer.write(json.dumps(metadata).encode() + b"\n" + data)
    except (OSError, ValueError, IndexError, struct.error, subprocess.SubprocessError):
        sys.exit(1)
