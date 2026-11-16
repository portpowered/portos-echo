"""Validate raw LLVM inputs before the existing coverage report merges them."""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


TOOLCHAIN = "nightly-2026-11-05"


def profdata_tool():
    """Use the same LLVM version as the coverage compiler, or its explicit override."""
    if os.environ.get("LLVM_PROFDATA"):
        return os.environ["LLVM_PROFDATA"]
    directory = subprocess.check_output(
        ["rustc", f"+{TOOLCHAIN}", "--print", "target-libdir"], text=True
    ).strip()
    suffix = ".exe" if os.name == "nt" else ""
    return str(Path(directory).parent / "bin" / f"llvm-profdata{suffix}")


def annotation(value):
    return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def module_signature(name):
    match = re.fullmatch(r"portos-echo-\d+-(\d+)(?:_\d+)?\.profraw", name)
    return match[1] if match else "unknown"


def validate(directory, quarantine, tool):
    """Keep good inputs byte-for-byte; retain bad inputs outside the merge glob."""
    subprocess.run([tool, "--version"], check=True, capture_output=True)
    profiles = sorted(directory.glob("*.profraw"))
    bad = []
    with tempfile.TemporaryDirectory(prefix="coverage-profile-") as temporary:
        for profile in profiles:
            result = subprocess.run(
                [tool, "merge", "-sparse", str(profile), "-o",
                 str(Path(temporary) / "checked.profdata")],
                capture_output=True, text=True,
            )
            if result.returncode:
                bad.append((profile, result.stderr.strip() or result.stdout.strip()))
    if bad:
        quarantine.mkdir(parents=True, exist_ok=True)
        destination = Path(tempfile.mkdtemp(prefix="run-", dir=quarantine))
        for profile, error in bad:
            shutil.move(str(profile), destination / profile.name)
            print(f"::warning::module_signature={module_signature(profile.name)} "
                  f"file={annotation(profile.name)}: {annotation(error)}",
                  flush=True)
    if len(bad) > 1 or len(profiles) == len(bad):
        raise ValueError(
            "Systemic coverage profile problem: "
            f"quarantined={len(bad)}, remaining={len(profiles) - len(bad)}; "
            "refusing coverage report"
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile-dir", type=Path, default=Path("target/llvm-cov-target"))
    parser.add_argument("--quarantine", type=Path, default=Path("target/llvm-cov/quarantine"))
    parser.add_argument("--profdata")
    args = parser.parse_args()
    try:
        validate(args.profile_dir, args.quarantine, args.profdata or profdata_tool())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"coverage profiles: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
