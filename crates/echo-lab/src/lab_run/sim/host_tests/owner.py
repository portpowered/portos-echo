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
                observations = json.loads(audit.read_text()) if audit.exists() else []
                observations.append({"owner_completed": completed, "pid": self.proc.pid})
                audit.write_text(json.dumps(observations))

    start.__globals__["emit"] = capture
    host.start, host.close = observed_start, observed_close


def runtime_observer():
    namespace = runpy.run_path(str(Path(__file__).with_name("runtime_owner.py")))
    return namespace["Runtime"]()


def guest(script, directory):
    namespace = runpy.run_path(script)
    audit = Path(script).resolve().parents[5] / ".cache" / directory / "host.json"
    instrument(namespace, audit)
    runtime = runtime_observer()
    command = namespace["Host"].command

    def registered(self, value):
        if value.get("action") == "registered":
            runtime.registered()
        return command(self, value)

    namespace["Host"].command = registered
    try:
        return namespace["main"]()
    finally:
        runtime.close()


def main():
    directory, *arguments = sys.argv[1:]
    arguments = [arg for arg in arguments if arg != "-u"]
    script, *flags = arguments
    sys.argv = [script, *flags]
    if script == "-c":
        sys.argv = ["-c", *flags[1:]]
        exec(compile(flags[0], "<string>", "exec"), {"__name__": "__main__"})
        return 0
    if Path(script).name != "host.py":
        # run_path leaves the adapter directory on sys.path, unlike a direct
        # Python script launch. Providers import modules beside their script.
        sys.path[0] = str(Path(script).resolve().parent)
        runpy.run_path(script, run_name="__main__")
        return 0
    return guest(script, directory)


if __name__ == "__main__":
    sys.exit(main())
