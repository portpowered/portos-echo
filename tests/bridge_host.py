"""Bridge T3 observer of the pinned host's synthetic playback file."""
import tempfile

from wyoming_host import Host


def main():
    with tempfile.TemporaryDirectory(prefix="bridge-t3-") as folder:
        host = Host(folder)
        try:
            host.start()
            print(host.port, flush=True)
            import sys

            for command in sys.stdin:
                if command.strip() == "pcm":
                    data = (host.folder / "play.raw").read_bytes()
                    print(f"{len(data)} {sum(byte != 0 for byte in data)}", flush=True)
                else:
                    host.command(command.strip())
        finally:
            host.stop()
    print("cleaned", flush=True)


if __name__ == "__main__":
    main()
