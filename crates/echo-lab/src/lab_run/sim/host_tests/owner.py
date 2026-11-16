"""Private T3 Python executable adapter; retain the actual host's Linux pidfd."""
import json
import os
from pathlib import Path
import runpy
import select
import signal
import subprocess
import sys


def instrument(namespace, audit):
    host = namespace["Host"]
    start, close = host.start, host.close
    emit = start.__globals__["emit"]
    ready = []

    def capture(value):
        if value["event"] == "host.ready":
            ready.append(value)
        else:
            emit(value)

    def observed_start(self):
        self.owner_fd = None
        start(self)
        self.owner_fd = os.pidfd_open(self.proc.pid)
        emit(ready.pop())

    def observed_close(self):
        try:
            close(self)
        finally:
            fd = getattr(self, "owner_fd", None)
            if fd is not None:
                # Observe before fixture cleanup. A leaked real process must
                # fail the assertion even though this adapter then collects it.
                completed = bool(select.select([fd], [], [], 0)[0])
                if not completed:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                    self.proc.wait(timeout=4)
                os.close(fd)
                self.owner_fd = None
                audit.write_text(json.dumps({"owner_completed": completed,
                                             "pid": self.proc.pid}))

    start.__globals__["emit"] = capture
    host.start, host.close = observed_start, observed_close


def guest(script, directory):
    namespace = runpy.run_path(script)
    audit = Path(script).resolve().parents[5] / ".cache" / directory / "host.json"
    instrument(namespace, audit)
    return namespace["main"]()


def main():
    directory, *arguments = sys.argv[1:]
    arguments = [arg for arg in arguments if arg != "-u"]
    script, *flags = arguments
    sys.argv = [script, *flags]
    if os.name != "nt":
        return guest(script, directory)
    namespace = runpy.run_path(script)
    popen = subprocess.Popen
    # Keep the delivered Windows launcher/guest collection behavior. Substitute
    # only its Python guest entry to install the same pidfd observer in Linux.
    code = ("import runpy,sys; from pathlib import Path; "
            "ns=runpy.run_path(str(Path(sys.argv[1]).parent/'host_tests/owner.py')); "
            "sys.exit(ns['guest'](sys.argv[1],sys.argv[2]))")

    def boundary(argv, **kwargs):
        if "python3" in argv:
            index = argv.index("python3") + 2
            argv = argv[:index] + ["-c", code, argv[index], directory, *argv[index + 1:]]
        return popen(argv, **kwargs)

    namespace["launch_windows"].__globals__["subprocess"].Popen = boundary
    return namespace["launch_windows"]()


if __name__ == "__main__":
    sys.exit(main())
