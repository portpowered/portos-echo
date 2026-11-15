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
