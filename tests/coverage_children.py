"""Collect coverage-only child profiles according to observed process exits."""

from pathlib import Path
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from coverage_profiles import profdata_tool, validate


def collect(root, inputs):
    """Promote every natural exit byte-for-byte; retain killed children in place."""
    inputs.mkdir(parents=True, exist_ok=True)
    counts = {"natural": 0, "killed": 0, "not-started": 0}
    if not root.exists():
        return counts
    for child in sorted(root.iterdir()):
        if not child.is_dir():
            raise ValueError(f"unexpected child coverage entry: {child}")
        disposition = (child / "exit").read_text()
        if disposition not in counts:
            raise ValueError(f"unresolved child exit: {child}")
        counts[disposition] += 1
        if disposition == "not-started" and list(child.glob("*.profraw")):
            raise ValueError(f"unstarted child has profiles: {child}")
        if disposition == "natural":
            for profile in child.glob("*.profraw"):
                destination = inputs / f"child-{root.name}-{child.name}-{profile.name}"
                if destination.exists():
                    raise ValueError(f"duplicate natural profile: {destination}")
                shutil.move(str(profile), destination)
    return counts


def check_natural(root, inputs, quarantine, tool):
    """Reject even one corrupt natural child, retaining the unchanged guard's evidence."""
    for profile in inputs.glob(f"child-{root.name}-*.profraw"):
        result = subprocess.run([tool, "show", str(profile)], capture_output=True, check=False)
        if result.returncode:
            # Run the existing guard, including quarantine and its unchanged
            # systemic-refusal threshold. The writer oracle is deliberately stricter.
            validate(inputs, quarantine, tool)
            raise ValueError("corrupt natural child profile; refusing coverage report")


if __name__ == "__main__":
    import os
    root = os.environ.get("ECHO_COVERAGE_CHILD_PROFILES")
    if root:
        print(f"coverage child exits: {collect(Path(root), Path('target/llvm-cov-target'))}")
        check_natural(Path(root), Path("target/llvm-cov-target"),
                      Path("target/llvm-cov/quarantine"), profdata_tool())
