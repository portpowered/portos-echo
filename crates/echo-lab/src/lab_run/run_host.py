"""Private local-only host operations; no accounts, services, or device access."""

import datetime
import hashlib
import json
import sys
import wave


def main():
    operation, *args = sys.argv[1:]
    if operation == "clock":
        return {"hour": datetime.datetime.now().hour}
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
        times = [datetime.datetime.fromisoformat(t.replace("Z", "+00:00"))
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
