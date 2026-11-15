"""Scan Git-selected current sources without traversing ignored build output."""

import os
from pathlib import Path, PurePosixPath
import stat
import subprocess
import tempfile


def selected_paths(output):
    """Git's NUL records preserve spaces, newlines and Unicode; cached wins."""
    if output and not output.endswith(b"\0"):
        raise ValueError("incomplete Git selection")
    return dict.fromkeys(os.fsdecode(name) for name in output.split(b"\0") if name)


def copy_source(root, destination, name):
    relative = PurePosixPath(name)
    if not relative.parts or relative.is_absolute() or any(
            part in ("..", ".git") for part in relative.parts):
        raise ValueError("invalid selected path")
    source = root
    # Never follow links, including a parent replaced by a directory symlink.
    for part in relative.parts:
        source = source / part
        try:
            mode = source.lstat().st_mode
        except FileNotFoundError:
            return  # Current working-tree deletion; not an index-blob scan.
        if stat.S_ISLNK(mode) or source.is_junction():
            return
    if not stat.S_ISREG(mode):
        raise ValueError("unsupported selected entry")
    target = destination.joinpath(*relative.parts)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(source.read_bytes())


def scan(root):
    selection = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z",
         "--", ".", ":(top,exclude)factory/"],
        cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
    environment = dict(os.environ)
    # Relative explicit configuration is relative to the caller, not the snapshot.
    if environment.get("GITLEAKS_CONFIG"):
        environment["GITLEAKS_CONFIG"] = str(
            (root / environment["GITLEAKS_CONFIG"]).absolute())
    print("secret scan: Git-selected working-tree sources; excluded boundary: factory/",
          flush=True)
    with tempfile.TemporaryDirectory(prefix="echo-secret-sources-") as directory:
        snapshot = Path(directory)
        for name in selected_paths(selection.stdout):
            copy_source(root, snapshot, name)
        return subprocess.run(
            ["gitleaks", "dir", ".", "--no-banner", "--redact=100", "--verbose"],
            cwd=snapshot, env=environment, check=False).returncode


def main():
    try:
        return scan(Path.cwd())
    except (OSError, ValueError, subprocess.SubprocessError):
        # Do not echo exception text: filenames or tool output can contain secrets.
        print("secret scan failed: source selection, snapshot or scanner error", flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
