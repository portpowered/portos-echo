"""Private local-only host operations; no accounts, services, or device access."""

import datetime
import hashlib
import json
import re
import sys
import wave
import time


def instant(value):
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})", value):
        raise ValueError("timestamp requires RFC 3339 offset")
    parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp requires offset")
    return parsed


def wait_until(value):
    target = instant(value)
    fraction = re.search(r"\.(\d+)", value)
    # datetime truncates beyond microseconds; round the eligibility boundary up.
    if fraction and any(digit != "0" for digit in fraction[1][6:]):
        target += datetime.timedelta(microseconds=1)
    while True:
        remaining = (target - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
        if remaining <= 0:
            return {"reached": True}
        time.sleep(min(remaining, 0.1))


def main():
    operation, *args = sys.argv[1:]
    if operation == "wait_until":
        return wait_until(args[0])
    if operation == "wake_wait":
        # Use the playback timestamp on the host wall-clock boundary only.
        # A ten-second capture may already have consumed the whole wake window.
        end = instant(args[0]) + datetime.timedelta(seconds=6)
        return wait_until(end.isoformat())
    if operation == "wake_remaining":
        end = instant(args[0]) + datetime.timedelta(seconds=6)
        remaining = (end - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
        # Round up so the inclusive decision never shortens the window.
        import math
        return {"remaining_ms": max(0, math.ceil(remaining * 1000))}
    if operation == "pixels":
        return compare_pixels(*args)
    if operation == "wav":
        with wave.open(args[0], "rb") as source:
            frames = source.getnframes()
            if frames <= 0 or len(source.readframes(frames)) != frames * source.getnchannels() * source.getsampwidth():
                raise ValueError("empty or truncated WAV")
        return {"reply_frames": frames}
    if operation == "clock":
        return {"hour": datetime.datetime.now().hour, "monotonic_ms": int(time.monotonic() * 1000)}
    if operation == "wait":
        time.sleep(int(args[0]) / 1000)
        return {"waited_ms": int(args[0])}
    if operation == "digest":
        digest = hashlib.sha256()
        for path in args:
            with open(path, "rb") as source:
                contents = source.read()
            digest.update(len(contents).to_bytes(8, "big"))
            digest.update(contents)
        return {"sha256": digest.hexdigest()}
    if operation == "reply":
        path, started, ended, recorded, stopped = args
        times = [instant(t)
                 for t in [started, ended, recorded, stopped]]
        play_start, play_end, record_start, record_end = times
        if not record_start <= play_start <= play_end < record_end:
            raise ValueError("invalid capture timing")
        with wave.open(path, "rb") as source:
            params = source.getparams()
            skip = int((play_end - record_start).total_seconds() * params.framerate)
            if skip >= params.nframes:
                raise ValueError("no post-playback audio")
            source.setpos(skip)
            frames = source.readframes(params.nframes - skip)
        with wave.open(path, "wb") as target:
            target.setparams(params)
            target.writeframes(frames)
        return {"reply_frames": params.nframes - skip}
    raise ValueError("unsupported host operation")


if __name__ == "__main__":
    print(json.dumps(main()))
