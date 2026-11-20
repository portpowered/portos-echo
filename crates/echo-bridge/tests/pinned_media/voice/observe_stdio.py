"""Forward synthetic test traffic to the pin and record matched start replies."""
import json
import os
import sys
import threading


def relay(to_pin, from_pin, capture):
    pending = {}

    def forward_requests():
        with os.fdopen(to_pin, "wb", buffering=0) as sink:
            try:
                for line in sys.stdin.buffer:
                    request = json.loads(line)
                    if request.get("method") in ("thread/start", "thread/realtime/start"):
                        pending[request["id"]] = request
                    sink.write(line)
            except BrokenPipeError:
                pass

    threading.Thread(target=forward_requests, daemon=True).start()
    with os.fdopen(from_pin, "rb") as source, open(capture, "a", encoding="utf-8") as log:
        for line in source:
            reply = json.loads(line)
            if "result" in reply or "error" in reply:
                request = pending.pop(reply["id"], None)
                if request is not None:
                    log.write(json.dumps({"request": request, "reply": reply}) + "\n")
                    log.flush()
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()


pin_read, to_pin = os.pipe()
from_pin, pin_write = os.pipe()
if os.fork() == 0:
    os.close(pin_read)
    os.close(pin_write)
    relay(to_pin, from_pin, CAPTURE)
    os._exit(0)
os.close(to_pin)
os.close(from_pin)
os.dup2(pin_read, 0)
os.dup2(pin_write, 1)
os.close(pin_read)
os.close(pin_write)
# The bridge continues to own and reap the actual app-server PID.
os.execv(PIN, [PIN, *sys.argv[1:]])
