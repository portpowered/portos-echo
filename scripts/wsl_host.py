"""Private child collection shared by the Windows host launcher."""
import subprocess
import time


class LauncherError(RuntimeError):
    """Only fixed, sanitized launcher-stage diagnostics belong here."""


class HostCancelled(RuntimeError):
    """Caller EOF interrupts work; the cleanup result still comes from close."""


class LauncherChild:
    """Keep the handle until collection, including setup/forwarding failures.

    Four seconds includes the guest Host's three-second group grace and stays
    inside the Rust owner's five seconds. Closing stdin requests guest cleanup;
    terminating a wsl client alone is never evidence of guest collection.
    """
    def __init__(self, proc):
        self.proc = proc
        self.deadline = None
        self.status = None
        self.forced = False
        self.failure = None

    def exited(self):
        try:
            return self.proc.poll() is not None
        except OSError:
            raise LauncherError("cleanup: cannot observe launcher child") from None

    def close(self):
        if self.status is not None:
            if self.failure:
                raise LauncherError(self.failure)
            return self.status
        if self.deadline is None:
            self.deadline = time.monotonic() + 4
            try:
                self.proc.stdin.close()
            except (OSError, ValueError):
                self.failure = "cleanup: cannot close launcher input"
        try:
            self.status = self.proc.wait(timeout=max(0, self.deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            if not self.forced:
                try:
                    self.proc.kill()
                except OSError:
                    raise LauncherError("cleanup: cannot force launcher child") from None
                self.forced = True
            try:
                self.status = self.proc.wait()
            except OSError:
                raise LauncherError("cleanup: cannot collect launcher child") from None
        except OSError:
            raise LauncherError("cleanup: cannot collect launcher child") from None
        if self.failure:
            raise LauncherError(self.failure)
        return self.status
