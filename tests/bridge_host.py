"""Bridge T3 observer of the pinned host's synthetic playback file."""
import tempfile
import importlib.util
import time
import signal
from pathlib import Path

from wyoming_host import Host


def playback(folder):
    spec = importlib.util.spec_from_file_location(
        "barge_measure", Path(__file__).resolve().parents[1] / "scripts/measure-barge-in.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Playback(folder)


def cut_ready(sink):
    with sink.condition:
        if not sink.condition.wait_for(lambda: sink.start is not None, 5):
            raise TimeoutError("sink did not start")
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
                    cut = cut_ready(sink)
                    print("cut-ready", flush=True)
                elif command.strip() == "cut-result":
                    print(result(sink, cut), flush=True)
                elif command.strip() == "pcm":
                    data = (host.folder / "play.raw").read_bytes()
                    print(f"{len(data)} {sum(byte != 0 for byte in data)}", flush=True)
                elif command.strip() == "wake" and awake:
                    host.proc.send_signal(signal.SIGUSR1)
                    print("wake", flush=True)
                else:
                    host.command(command.strip())
                    awake = awake or command.strip() == "wake"
        finally:
            host.stop()
            if sink is not None:
                sink.close()
    print("cleaned", flush=True)


if __name__ == "__main__":
    main()
