"""Bridge T3 observer of the pinned host's synthetic playback file."""
import tempfile
import importlib.util
import time
import signal
import os
import select
from pathlib import Path

from wyoming_host import Host


def playback(folder):
    spec = importlib.util.spec_from_file_location(
        "barge_measure", Path(__file__).resolve().parents[1] / "scripts/measure-barge-in.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Playback(folder)


def cut_ready(sink, host):
    with sink.condition:
        if not sink.condition.wait_for(lambda: sink.start is not None, 5):
            logs = bytearray(host.log[-8192:])
            for _ in range(8):
                if not select.select([host.proc.stderr], [], [], 0)[0]:
                    break
                part = os.read(host.proc.stderr.fileno(), 8192)
                if not part:
                    break
                logs.extend(part)
            raise TimeoutError(
                f"sink did not start: host_exit={host.proc.poll()} "
                f"sink_bytes={len(sink.data)} host_log={logs[-8192:].decode(errors='replace')}")
        deadline = sink.start + 1
        while time.monotonic() < deadline:
            sink.condition.wait(timeout=deadline - time.monotonic())
    # Timestamp before instructing the fake to emit cancellation. This includes
    # the controller/fake dispatch latency rather than subtracting it away.
    return time.monotonic()


def result(sink, cut):
    with sink.condition:
        if not sink.condition.wait_for(lambda: sink.end is not None, 5):
            raise TimeoutError("sink never closed")
    assert 0 < len(sink.data) <= 144000
    assert not any(sink.data), "synthetic silence changed"
    return 1000 * (sink.end - cut)


def main():
    with tempfile.TemporaryDirectory(prefix="bridge-t3-") as folder:
        host = Host(folder)
        sink = None
        cut = None
        awake = False
        try:
            host.start()
            print(host.port, flush=True)
            import sys

            for command in sys.stdin:
                if command.strip() == "arm":
                    if sink is not None:
                        sink.close()
                    (host.folder / "play.raw").unlink(missing_ok=True)
                    sink = playback(folder)
                    print("armed", flush=True)
                elif command.strip() == "cut-ready":
                    try:
                        cut = cut_ready(sink, host)
                        print("cut-ready", flush=True)
                    except TimeoutError as error:
                        print(error, file=sys.stderr, flush=True)
                        print("sink-start-failed", flush=True)
                elif command.strip() == "cut-result":
                    print(result(sink, cut), flush=True)
                elif command.strip() == "pcm":
                    data = (host.folder / "play.raw").read_bytes()
                    print(f"{len(data)} {sum(byte != 0 for byte in data)}", flush=True)
                elif command.strip() == "wake-playing":
                    # Signal delivery alone does not prove the wake ran before
                    # Live cancellation. Wait for the real host's duplex branch
                    # while playback remains open, then measure the client cut.
                    host.deadline = time.monotonic() + 5
                    host.proc.send_signal(signal.SIGUSR1)
                    host.logged(b"wake word during the reply: left to the client (-D)")
                    cut = time.monotonic()
                    print("wake-playing", flush=True)
                elif command.strip() == "wake" and awake:
                    host.proc.send_signal(signal.SIGUSR1)
                    print("wake", flush=True)
                else:
                    if command.strip() == "restart":
                        host.deadline = time.monotonic() + 20
                        awake = False
                    host.command(command.strip())
                    awake = awake or command.strip() == "wake"
        finally:
            host.stop()
            if sink is not None:
                sink.close()
    print("cleaned", flush=True)


if __name__ == "__main__":
    main()
