---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active
---

# Echo Lab static harness

Run from the repository root:

```sh
cargo run -p echo-lab -- validate
cargo run -p echo-lab -- validate tests/scenarios/m0-harness.yaml
cargo run -p echo-lab -- check --milestone M0
```

`validate` checks explicit YAML files or the sorted `tests/scenarios/*.yaml` set
against the authored Draft 2020-12 scenario schema. Each file prints `ok` or
file/JSON-pointer/error diagnostics. Missing properties name the missing property;
root, read and parse errors use the empty pointer. It continues after failures and
returns 1 for any failure or empty discovery. Schemas are embedded; validation
uses no network. `--help` prints the command grammar.

`check --milestone M0` probes all twelve Phase F targets with real `make -n`, then
checks for `.github/workflows/ci.yml`. It prints every result and returns 1 if any
check fails. The missing workflow is expected until el-f10. This inventory does
not prove that gates pass or that M0 is complete. Use a trusted repository:
GNU Make may evaluate makefile expressions even during a dry run.

## Firmware probe

Build both executables once from the repository root, then run the synthetic probe:

```sh
cargo build -p echo-lab --bins --locked
target/debug/echo-lab probe --adb /absolute/path/to/target/debug/echo-lab-fake-adb --out /tmp/new-probe
```

On Windows, both executable names end in `.exe`. Supply an explicit executable
path; there is no default adb lookup. The output directory must be new and its
parent must exist. Keep probe artifacts outside the repository.

`probe` discovers both serial properties in memory before collecting the fixed
read-only observations. It refuses products other than `athena` with exit 2.
Success (exit 0) prints the same JSON saved as `manifest.json`: UTC timestamp,
tool version, build, six library MD5 comparisons, named input capabilities,
effective display size/density, the named amplifier control values and sorted
keyword-directory entries. Hash differences are observations, never permission
to install anything. Directory entries do not prove model contents.

Every captured stdout/stderr is scrubbed of both known nonempty serials before
storage, including discovery output. Discovery failure discards child output.
Sanitized raw records are numbered in fixed collection order as
`raw/<index>.stdout.txt` and `raw/<index>.stderr.txt`; `raw/.gitignore` contains
exactly `*` followed by a newline. This protects against accidental additions;
do not force-add raw artifacts. Redaction covers the two discovered serial
values; it cannot discover unrelated identifiers embedded in arbitrary output.

Bad arguments, adb failures, malformed observations or local write failures
return 1. Existing destinations are never overwritten, including concurrent
callers. `manifest.json` is published last through a same-directory rename and
is the completion marker. A file-write failure may leave sanitized incomplete
raw files. A stdout failure after publication returns 1 with the completed file
still available. There is no retry, watchdog or device latency guarantee;
the operator handles interruption of a stalled real adb.

Workers and reviewers use only the synthetic fake, which never invokes adb.
Tests use Cargo's prebuilt binaries and isolated fixture overrides/command
ledgers (`ECHO_LAB_FAKE_FIXTURE` and `ECHO_LAB_FAKE_LEDGER` on the fake only).
Only the coordinator may run against real hardware under the program's T5
authorization. Fake tests prove the CLI and persisted contract, not Android
permissions, firmware output variants or real-device cleanup.

The six-line 7080 hash reference is copied verbatim from
[echo-dot-assist's pinned athena profile](https://github.com/Gamer92000/echo-dot-assist/blob/8646a4f2f4aeafde2d7aca7359d9eff555a169c0/devices/athena/probe.md5).
That upstream reference is MIT licensed; its
[copyright and permission notice](https://github.com/Gamer92000/echo-dot-assist/blob/8646a4f2f4aeafde2d7aca7359d9eff555a169c0/LICENSE)
applies to the copied reference. These are the 7080 values, not the measured
7809 firmware values.

## Artifact contract

The report schema preserves the execution plan fields, adds tier, driver,
scenario digest, hassmic commit and bench session ids, and fixes placement to
`bridge_pc`. Device and hassmic commit may be null for host-only runs. Manual
observations require observer and observation details. Static commands emit no
execution report and create no artifact directory.

```text
artifacts/<milestone>/<run-id>/
  report.json
  events.jsonl
  mcp_calls.jsonl
  screen_recording.mp4
  <audio-metrics>.json
```

A run id identifies a timestamped run. Evidence paths are relative to that run's
directory. Future drivers create only applicable artifacts: redacted event/tool
traces, screen recording for animation, and derived audio metrics when applicable.
No raw credentials or unnecessary voice recordings belong in these artifacts.

W1 owns driver semantics, execution and artifact creation. Screen, key, touch and
injection payloads currently receive structural validation only. T0-T2 tests use
the prebuilt binary, isolated temporary directories, local schemas and real make
dry runs; no hardware, remote account or conversation behavior is exercised.
