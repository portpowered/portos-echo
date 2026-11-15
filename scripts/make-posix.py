"""Read-only discovery of the selected Git for Windows POSIX tools for Make."""

from pathlib import Path
import shutil
import sys


REQUIRED = ("sh", "xargs", "cp", "diff", "grep", "mkdir", "mktemp", "rm", "tr", "wc")
REPAIR = "Install or repair Git for Windows manually and put its git.exe on PATH."


def discover(path, lookup=shutil.which):
    """Use only the first Git on the supplied PATH; never execute a probe."""
    git = lookup("git.exe", path=path)
    if not git:
        raise ValueError(f"git.exe not found on inherited PATH. {REPAIR}")
    executable = Path(git).resolve()
    parent = executable.parent
    if parent.name.lower() in ("cmd", "bin"):
        root = parent.parent
        if root.name.lower() in ("usr", "mingw64") and parent.name.lower() == "bin":
            root = root.parent
    else:
        raise ValueError(f"Unsupported git.exe layout: {executable.as_posix()}. {REPAIR}")
    tools = root / "usr" / "bin"
    missing = [name + ".exe" for name in REQUIRED if not (tools / (name + ".exe")).is_file()]
    if missing:
        raise ValueError(f"Missing {', '.join(missing)}; checked {tools.as_posix()}. {REPAIR}")
    return tools.as_posix()


def main(args):
    import os

    try:
        if args:
            raise ValueError("Expected no arguments: python scripts/make-posix.py")
        if os.name != "nt":
            raise ValueError("make-posix.py is for Windows; use native sh on other hosts")
        print(discover(os.environ.get("PATH", "")))
        return 0
    except (OSError, ValueError) as error:
        print(f"POSIX bootstrap: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
