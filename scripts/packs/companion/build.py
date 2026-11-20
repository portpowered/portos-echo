"""Publish a reproducible format-1 Companion directory and download archive."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile

from render import BACKGROUND, FRAMES, render

MIB = 1024 ** 2
DIAGNOSTIC = "screenpack: generation failed; output was not published"


def clip(name, mode):
    return {"webm": f"{name}.webm", "mp4": f"{name}.mp4", "mode": mode}


def manifest(version):
    states = {role: {"clip": clip(role, "loop")}
              for role in FRAMES if role not in ("happy", "setup-enter")}
    states["setup"]["enter"] = clip("setup-enter", "once")
    return {
        "format": 1,
        "name": "companion",
        "version": version,
        "layout": {
            "safe_area": {"x": 0, "y": 0, "width": 100, "height": 100},
            "slots": {
                "clock": {"x": 46, "y": 82},
                "date": {"x": 45, "y": 91},
                "status": {"x": 70, "y": 35, "font_scale": 1},
                "link_code": {"x": 70, "y": 53},
                "approval": {"x": 70, "y": 53},
                "error": {"x": 70, "y": 53},
            },
            "stage": {"anchor": {"x": 42, "y": 41}, "scale": 1.62,
                      "background": BACKGROUND},
        },
        "character": {
            "rest": "rest.png",
            "states": states,
            "emotes": {"happy": clip("happy", "once")},
        },
    }


def archive_pack(pack, archive):
    files = sorted(pack.iterdir())
    if (len(files) > 64 or sum(path.stat().st_size + 1024 for path in files) > 64 * MIB
            or any(path.stat().st_size > 8 * MIB for path in files
                   if path.suffix in (".mp4", ".webm"))):
        raise ValueError("media exceeds archive limits")
    with archive.open("wb") as stream, gzip.GzipFile(
            filename="", mode="wb", fileobj=stream, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as output:
            for path in files:
                data = path.read_bytes()
                entry = tarfile.TarInfo(path.name)
                entry.mode = 0o644
                entry.size = len(data)
                output.addfile(entry, io.BytesIO(data))
    if archive.stat().st_size > 64 * MIB:
        raise ValueError("compressed archive exceeds download limit")
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    archive.with_suffix(".gz.sha256").write_text(f"{digest}  {archive.name}\n", encoding="ascii")


def build(version, out):
    if sys.platform != "linux" or out.exists() or out.is_symlink():
        raise ValueError("Linux and an unused output path are required")
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".companion-", dir=out.parent) as directory:
        stage = Path(directory)
        payload = stage / "output"
        pack = payload / f"companion@{version}"
        pack.mkdir(parents=True)
        render(pack, stage)
        (pack / "pack.json").write_text(json.dumps(manifest(version), indent=2) + "\n",
                                       encoding="utf-8")
        archive_pack(pack, payload / f"companion-{version}.screenpack.tar.gz")
        # Same filesystem as out. Refuse a destination created mid-build.
        subprocess.run(["mv", "-T", "-n", "--", str(payload), str(out)],
                       capture_output=True, check=True)
        if payload.exists():
            raise ValueError("output was occupied during generation")


def main():
    parser = argparse.ArgumentParser(description="Build Companion screen-pack data (Linux only).")
    parser.add_argument("--version", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*)?", args.version):
        parser.error("invalid version")
    try:
        build(args.version, args.out.absolute())
    except (OSError, ValueError, subprocess.SubprocessError):
        print(DIAGNOSTIC, file=sys.stderr)
        return 1
    print(f"screenpack: companion={args.version} generated=PASS checksum=PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
