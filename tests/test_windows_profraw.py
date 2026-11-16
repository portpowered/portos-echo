"""T3 coverage witness using the prebuilt writer and its actual probe stream owner."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from coverage_children import check_natural, collect

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from coverage_profiles import profdata_tool, validate


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def check_profile(tool, profile):
    result = subprocess.run(
        [tool, "show", "--function=17echo_lab_fake_adb4main", str(profile)],
        capture_output=True, text=True, check=False,
    )
    require(result.returncode == 0, f"corrupt natural profile {profile}: {result.stderr}")
    return "17echo_lab_fake_adb4main" in result.stdout


def journey(binary, root):
    root.mkdir()
    env = {**os.environ, "ECHO_COVERAGE_CHILD_PROFILES": str(root / "children"),
           "LLVM_PROFILE_FILE": str(root / "owner-%p-%m.profraw")}
    for case in (
        "t3_handler_exact_report_log_prompts_and_read_only_ledger",
        "t3_stream_drop_reaps_and_joins_after_a_readiness_signal",
        "t3_report_io_and_adb_launch_failures_cannot_claim_success",
    ):
        result = subprocess.run(
            [str(binary), "--exact", f"lab_calibrate::calibrate_tests::{case}", "--nocapture"],
            env=env, capture_output=True, text=True, timeout=30, check=False,
        )
        require(result.returncode == 0 and "1 passed" in result.stdout,
                f"producer {case}: {result.stdout}\n{result.stderr}")
    return root / "children"


def exercise(writer, binary, root, tool, report_inputs):
    # Reuse already built artifacts. Two independent producer sessions overlap;
    # each contains ordinary reads and a readiness-signalled forced termination.
    with ThreadPoolExecutor(max_workers=2) as pool:
        children = list(pool.map(lambda n: journey(binary, root / f"case with spaces {n}"), range(2)))
    inputs = root / "inputs"
    for child in children:
        before = {p: p.read_bytes() for d in child.iterdir()
                  if (d / "exit").read_text() == "killed" for p in d.glob("*.profraw")}
        counts = collect(child, inputs)
        require(counts["natural"] >= 1 and counts["killed"] >= 1, f"missing exit witnesses: {counts}")
        require(all(p.read_bytes() == data for p, data in before.items()), "killed bytes changed")
        print(f"producer exits: {counts}; retained killed profiles={len(before)}", flush=True)
    profiles = list(inputs.glob("*.profraw"))
    require(profiles, "natural children contributed no profiles")
    require(any([check_profile(tool, p) for p in profiles]), "actual writer counters missing")
    # Ordinary direct execution must also contribute a nonempty valid writer profile.
    env = {**os.environ, "LLVM_PROFILE_FILE": str(root / "ordinary-%p-%m.profraw")}
    result = subprocess.run([str(writer), "shell", "getprop", "ro.product.device"],
                            env=env, capture_output=True, check=True, timeout=10)
    require(result.stdout == b"athena\n", "ordinary writer returned the wrong fixture")
    ordinary = list(root.glob("ordinary-*.profraw"))
    require(ordinary and all(p.stat().st_size > 0 for p in ordinary), "missing ordinary counters")
    require(all(check_profile(tool, p) for p in ordinary), "ordinary writer counters missing")
    print(f"ordinary writer: {len(ordinary)} nonempty LLVM-readable profiles", flush=True)
    root_failure(writer, root)
    # The producer tests themselves execute the ownership code. Preserve their
    # counters before TemporaryDirectory removes them; only positive cases enter
    # the report, never the intentional corruption used by the negative oracle.
    owners = list(root.glob("case with spaces */owner-*.profraw"))
    owners += list(root.glob("failure-owner-*.profraw"))
    require(owners, "producer ownership counters missing")
    report_inputs.mkdir(parents=True, exist_ok=True)
    for profile in owners:
        require(profile.stat().st_size > 0, f"empty owner profile: {profile}")
        result = subprocess.run([tool, "show", str(profile)], capture_output=True, check=False)
        require(result.returncode == 0, f"corrupt owner profile: {profile}: {result.stderr}")
        destination = report_inputs / f"writer-regression-{root.name}-{profile.name}"
        require(not destination.exists(), f"duplicate owner profile: {destination}")
        shutil.copyfile(profile, destination)
    print(f"producer ownership: {len(owners)} readable profiles retained for reporting", flush=True)
    negative_oracle(root, inputs, tool, ordinary[0])


def root_failure(writer, root):
    blocked = root / "blocked-root"
    blocked.write_text("keep")
    ledger = root / "failed-launch-ledger"
    env = {**os.environ, "ECHO_COVERAGE_CHILD_PROFILES": str(blocked),
           "LLVM_PROFILE_FILE": str(root / "failure-owner-%p-%m.profraw"),
           "ECHO_LAB_FAKE_LEDGER": str(ledger)}
    binary = writer.with_name("echo-lab.exe" if os.name == "nt" else "echo-lab")
    result = subprocess.run(
        [str(binary), "calibrate", "--adb", str(writer), "--out", str(root / "failed-output")],
        env=env, capture_output=True, text=True, timeout=10, check=False,
    )
    require(result.returncode == 1 and "coverage: cannot create profile root" in result.stderr,
            f"profile root failure escaped CLI: {result.stdout}\n{result.stderr}")
    require(not ledger.exists(), "profile root failure launched the writer")
    require(blocked.read_text() == "keep", "profile root failure changed existing bytes")
    print("profile root failure: CLI refused before writer launch; existing bytes retained", flush=True)


def negative_oracle(root, inputs, tool, good):
    # Classification must never depend on validity: corrupt natural bytes are
    # promoted unchanged, whereas even valid killed bytes remain diagnostic.
    ownership = root / "negative"
    for name, disposition, data in (
        ("natural-corrupt", "natural", b"corrupt header"),
        ("killed", "killed", good.read_bytes()),
    ):
        child = ownership / name
        child.mkdir(parents=True)
        (child / "exit").write_text(disposition)
        (child / "test.profraw").write_bytes(data)
    collect(ownership, inputs)
    corrupt = next(inputs.glob("*natural-corrupt*.profraw"))
    require(corrupt.read_bytes() == b"corrupt header", "corrupt natural input was suppressed")
    require((ownership / "killed/test.profraw").read_bytes() == good.read_bytes(), "killed bytes lost")
    result = subprocess.run([tool, "show", str(corrupt)], capture_output=True, check=False)
    require(result.returncode != 0, "single corrupt natural profile escaped strict oracle")
    quarantine = root / "single quarantine"
    try:
        check_natural(ownership, inputs, quarantine, tool)
    except ValueError:
        require(len(list(quarantine.rglob("*.profraw"))) == 1, "single corrupt natural profile not retained")
    else:
        raise AssertionError("single corrupt natural profile escaped collector oracle")
    # Preserve the unchanged general guard: two bad inputs refuse and quarantine.
    corrupt.write_bytes(b"corrupt header")
    shutil.copyfile(corrupt, inputs / "second-corrupt.profraw")
    quarantine = root / "quarantine"
    try:
        validate(inputs, quarantine, tool)
    except ValueError:
        require(len(list(quarantine.rglob("*.profraw"))) == 2, "guard did not quarantine both inputs")
    else:
        raise AssertionError("corrupt natural profiles did not trip unchanged guard")
    print("negative oracle: one corrupt natural profile rejected; two quarantined/refused", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--writer", type=Path, required=True)
    parser.add_argument("--probe-tests", type=Path)
    parser.add_argument("--profile-inputs", type=Path,
                        help="report input directory (default: prebuilt writer's target directory)")
    args = parser.parse_args()
    writer = args.writer.resolve()
    if os.name == "nt" and writer.suffix != ".exe":
        writer = writer.with_suffix(".exe")
    require(writer.is_file(), f"prebuilt writer unavailable: {writer}")
    suffix = ".exe" if os.name == "nt" else ""
    candidates = list((writer.parent / "deps").glob(f"probe_cli-*{suffix}"))
    candidates += list((writer.parent / "build/echo-lab").glob(f"*/out/probe_cli-*{suffix}"))
    candidates = [p for p in candidates if p.is_file() and (p.suffix == suffix if suffix else not p.suffix)]
    require(args.probe_tests or candidates, "prebuilt probe_cli test executable unavailable")
    binary = args.probe_tests.resolve() if args.probe_tests else max(candidates, key=lambda p: p.stat().st_mtime)
    with tempfile.TemporaryDirectory(prefix="writer coverage ") as temporary:
        exercise(writer, binary, Path(temporary), profdata_tool(),
                 args.profile_inputs.resolve() if args.profile_inputs else writer.parent.parent)
    print("writer profile regression: PASS", flush=True)


if __name__ == "__main__":
    main()
