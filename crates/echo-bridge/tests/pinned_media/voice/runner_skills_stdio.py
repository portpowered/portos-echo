#!/usr/bin/env python3
"""Relay stdio unchanged to the verified pin and observe synthetic skill RPCs."""
import json
import os
import pathlib
import sys
import threading


def relay(to_pin, from_pin, path):
    def requests():
        with os.fdopen(to_pin, 'wb', buffering=0) as destination, path.open('wb', buffering=0) as log:
            try:
                for row in sys.stdin.buffer:
                    frame = json.loads(row)
                    if frame.get('method') == 'thread/start' or 'contentItems' in (frame.get('result') or {}):
                        log.write(row)
                    destination.write(row)
            except BrokenPipeError:
                pass

    threading.Thread(target=requests, daemon=True).start()
    with os.fdopen(from_pin, 'rb') as source:
        for row in source:
            sys.stdout.buffer.write(row)
            sys.stdout.buffer.flush()


root = pathlib.Path(__file__).parent
pin = (root / 'pin-path').read_text()
pin_read, to_pin = os.pipe()
from_pin, pin_write = os.pipe()
if os.fork() == 0:
    os.close(pin_read)
    os.close(pin_write)
    relay(to_pin, from_pin, root / 'skills-rpc.jsonl')
    os._exit(0)
os.close(to_pin)
os.close(from_pin)
os.dup2(pin_read, 0)
os.dup2(pin_write, 1)
os.close(pin_read)
os.close(pin_write)
# The bridge supervises and reaps the actual pinned app-server PID.
os.execv(pin, [pin, *sys.argv[1:]])
