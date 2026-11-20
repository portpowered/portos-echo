---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 20
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

## Sim test bridge

The M4 component witnesses use the prebuilt configured `echo-bridge` binary,
the pinned host, synthetic Live and Responses peers, and the spoof MCP:

```sh
cargo test -p echo-lab --bin echo-lab t3_m4_pinned_host --locked -- --ignored --nocapture
```

Each fresh world sends one finite 100 ms synthetic capture through the host.
Its explicit `--finite` owner disables wake sounds and their removal, sends
the fixture once, and stops feeding the FIFO afterward. The transparent Wyoming
observer matches the complete sample-sign sequence across frames, preserving
the endpoint through the host's microphone gain. It measures the interval from
the successful socket write containing that endpoint to the first reply PCM
write. These are host-boundary observations, not DAC or acoustic timings.
The paired screen SSE must show ready, listening, speaking, and ready in order.
Success and HTTP 500 use independent worlds, exact model results, and checked
cleanup. The public M4 scenario uses these same owned worlds:

```sh
cargo build -p echo-bridge -p echo-lab --bins --locked
target/debug/echo-lab run tests/scenarios/m4-spoof-tool.yaml --driver sim
```

The unchanged utterance maps to the finite synthetic capture. The sim driver
selects the sibling production `echo-bridge`; `--bridge` explicitly overrides it.
W1 retains its helper. Discovery executes M4 too, and `make test-sim` runs it
once, without retries. Each case uses fresh peers and private
configuration; cleanup failures fail the run. `report.json` records actual
sent transcript, paired screen state, counts, PCM bytes and socket latency.
The p95 budget covers the positive and fresh HTTP 500 samples together.

`tests/golden/m4-success.jsonl` and `m4-http500.jsonl` were recorded from these
configured bridge worlds. The runner normalizes only incidental timestamps,
IDs and ephemeral endpoints, then compares every line. A difference fails and
prints expected/actual lines. There is no automatic update option. Normalized
actual traces are retained beside the report for inspection; private raw traces,
keys, configuration and PCM are removed. The explicit dialogue capture gate
orders the first microphone observation before delegation without sorting or
filtering the trace. Golden changes require a separately observed run and
review of each semantic difference.

`cargo build -p echo-lab --bin echo-lab-sim-bridge --locked` builds the
test-purpose Wyoming bridge. It has no model or tool execution. Supply
`ECHO_LAB_WYOMING_ADDR=127.0.0.1:<port>` and an absolute native
`ECHO_LAB_TRACE_PATH` naming a fresh JSONL file. Addresses are numeric IPv4
loopback only. An existing trace is refused without changing its bytes.

The owner keeps bridge stdin open during observation and closes it for clean
shutdown. Stdout emits `{"event":"bridge.ready"}` after registration and a
round-trip describe barrier. Each completed run produces exactly these trace
records, sharing a bridge-local `run_id` (`run-1`, `run-2`, ...):

```jsonl
{"event":"wyoming.run_started","elapsed_ms":0,"run_id":"run-1"}
{"event":"wyoming.mic_frames","elapsed_ms":700,"run_id":"run-1","count":16}
{"event":"wyoming.run_ended","elapsed_ms":700,"run_id":"run-1"}
```

Counts come from frames read through `echo-wyoming`, including frames received
while the bridge sends a 100 ms silent reply. After at least ten microphone
frames, that reply ends the host run; `played` and microphone closure must both
arrive before the terminal records. Dropped frames, short runs and protocol
errors fail with exit 1. No microphone `audio-start` is required or invented.
Audio bytes are discarded. Timestamps observe the host's real clock edge;
the 120-second timeout is only a safety ceiling.

After flushing the terminal trace, stdout emits
`{"event":"bridge.run_completed","run_id":"run-1"}` as a completion barrier.
Non-EOF stdin data fails. EOF stops the connection; owners must check their
expected terminal trace before requesting shutdown. A protocol failure exits
even when the owner still holds stdin open.

The component test in `make test-sim` uses the pinned prebuilt host through the
existing `tests/wyoming_host.sh` controller, with its default synthetic silence.
It proves the bridge's real Wyoming boundary. It does not yet execute scenario
steps or publish a sim report. The scenario driver, fixture feeding, fake
servers, observation oracles and discovery remain implementation work in this
lane. The upstream host's wildcard listeners remain an accepted limitation:
`pinned hassmic-host binds wildcard listeners (upstream; no bind-address option)`.

## M5 authorization counterpart

`echo_lab::lab_mcp::Fixture` owns one loopback authorization listener and one
synthetic grant. Bind it with the scenario's UTC epoch, pass `validator()` to
`Spoof::start_authorized`, then call `set_resource(spoof.url())` before linking.
Discovery on the spoof supplies the resource and authorization-server origin;
the authorization endpoints come from that server's metadata. The public
client id is `echo-lab`, with scope `endpoint:read`.

The owner selects pending, approved or denied through `approve`, and advances
fixture UTC explicitly with `advance`. Advance the bridge's paused clock by
the same amount; socket readiness must never automatically advance either
clock. `counts` reports device authorizations, polls, successful rotations, rejected
refresh replays and revokes without exposing credentials. Close the spoof and
then `shutdown` the fixture to join both listeners.

The counterpart checks ES256 proofs, endpoint/method/time, unique proof IDs and
the registered key. It issues signed access JWTs; the spoof validates their
issuer, audience, use, subject, scope, expiry and active grant before dispatch.
Refresh replay revokes that grant. An omitted resource deliberately produces
a first-party access token which MCP rejects; a wrong resource returns
`400 invalid_target`. MCP and revoke accept no DPoP proof.

`cargo test --locked -p echo-lab --lib` exercises this boundary with the real
auth client and private store. These component witnesses do not replace the
real bridge's M5 scenario or prove cloud/device compatibility.

Build the delivered bridge once, then run its grant integration witnesses:

```sh
cargo build --locked -p echo-bridge --bin echo-bridge
cargo test --locked -p echo-bridge --test bridge_t3 mcp_link -- --ignored --nocapture
```

The strict cases use this authorization counterpart, the signed-token spoof,
the prebuilt configured bridge, its public operator commands, paired screen,
and controlled Live/model/Wyoming peers. They prove a crash after rotation,
restart and a second refresh followed by authenticated `test.echo` returning
`m5-linked`, replay rejection and the MCP relink latch, and healthy repeated
unlink. Resource admission independently reads the configured durable store
before accepting each Bearer, so premature access publication fails the test.
Additional cases inject failed storage replacement, failed auth transport and
cancellation after remote rotation; the owner must block further refresh and
never publish an uncertain completion. All state is in isolated temporary
directories outside the repository. These tests do not use `hassmic-host` or
produce the M5 scenario report; those remain separate acceptance evidence.

`echo_lab::lab_mcp::Observations` binds M5 measurements to those independent
boundaries. Feed complete bridge trace records once and call `restarted` only
after collecting an actual prior bridge process. Operation IDs are scoped to
each process. Feed paired screen states and the delegated model's actual
requests through `screen` and `model`; model call IDs are scoped to delegation,
and repeated conversation history does not duplicate a result. `measure`
reconciles lifecycle completions with authorization counts and the signed-spoof
snapshot, returning the same `LinkCounts` type used by the report oracle.

A spoof arrival is not a successful tool result: injected failures also record
arrivals. Only the exact returned `m5-linked` tool output counts as a success,
and it must have a corresponding admitted spoof dispatch. Restart evidence
requires both a reload observation and successful use in the new process.
Repeated identical relink screen states count as one transition. Malformed or
contradictory trace/model observations latch a fixed diagnostic across restarts.
Pending/denied authorizations and uncertain rotations never infer completions.
The strict delivered-bridge witness exercises this binding. The M5 pinned-host
runner also consumes lifecycle and paired-screen observations for its mounted
operator controls, external refresh replay and delivered model outputs. The authored
`m5-port-os-link` scenario uses the same binding. Restart alone never counts as
live grant reuse.

## Device runner

The device runner supports batches of speech, screen and frame steps, firmware
admission, quiet hours, held-session cleanup, reply capture and reports:

```sh
echo-lab run scenario.yaml --driver device --bench ring --port /explicit/port --adb /explicit/adb --stt stt-wrapper --ring fixture-ring --out /external/new-directory
```

`say: utterance.wav` selects a WAV relative to the scenario directory. Missing
files fail before tools or artifacts are created. This lane commits no utterance
audio. Optional `say: "text:synthetic utterance"` requires both
`--speech-endpoint` and `--speech-model`; there are no speech defaults.
`--ring` overrides `ECHO_LAB_RING`. Port and adb paths must name existing files
explicitly; bare executable names are refused. Arguments go directly to child
processes, without shell interpretation. `--stt` is one executable (or a wrapper
for additional options), invoked with the reply WAV path as exactly one argument.
Its stdout is a UTF-8 transcript; transcripts and raw tool diagnostics are never
persisted. `expect_speech_contains` checks literal, case-insensitive substrings.
An explicit `regex:` prefix selects a pattern; invalid/empty expectations fail
before effects. Pattern matching uses the existing schema validator's regex
engine, with no extra package dependency.

`expect_screen: {golden: tests/goldens/example.png, tolerance: 0}` captures
`adb exec-out screencap -p`, preserves the raw PNG, and compares decoded RGBA
pixels to the golden. Goldens must resolve inside the repository's
`tests/goldens/`. Tolerance is exactly zero differing pixels. The bounded Python
standard-library decoder handles non-interlaced 8-bit gray, gray-alpha, palette,
RGB and RGBA PNGs; unsupported formats and corrupt inputs fail explicitly.

`expect_frame: frames/ring.jpg` saves a snapshot from the same held lease.
The approved CLI source returns JPEG bytes and a capture timestamp, rather
than PNG. Paths must be portable relative paths without traversal; existing
destinations and symlink parents are refused. The runner checks the capture
confirmation and JPEG structure, retaining exact bytes as report evidence.
The file must start with SOI and end with EOI; segment lengths must fit, with
exactly one SOF0/SOF1/SOF2 before SOS, positive dimensions, one or three
components, DQT and DHT tables, and at least one entropy byte after SOS.
This is capture evidence only; it never asserts the Ring image's contents.
Entropy is not decoded: structurally intact files with garbage entropy pass.
The coordinator reads what the light ring shows from the frame at T5.

When a frame immediately follows `say`, play-only queues the clip without
`--listen`. The device command gate waits through the bounded host helper until
the wall clock reaches the returned `playEndedAt`, then takes the held snapshot
and starts `message record --audio-only --duration 10`. No subsequent held port
command (including status, release or a pending recording launch) can interrupt
that playback. The helper parses RFC 3339 offsets as instants and rechecks wall
time after each wait. Invalid timing or an unsuccessful wait fails closed;
cleanup reports an unconfirmed release if it cannot establish eligibility.
A launched play whose response is lost, cancelled, nonzero or oversized retains
unknown completion: no status, release, snapshot, record or new play is sent,
and `session_released=false` requires coordinator recovery. Reaping the local
CLI does not establish that remote audio stopped.
The speech oracle joins/checks the pending child before STT. Other utterances
use combined play/listen timestamps and trim the WAV at playback end.
Finalization and the next utterance join/check pending recordings before
releasing/reusing the session. Every child has cancellation and lease deadlines.

Every step reads the firmware incremental, `ro.bootmode` and
`sys.boot_completed` through explicit adb. Only listed builds in
`data/probed-builds.txt` with bootmode `normal` and boot-completed `1` are
admitted. Recovery or incomplete startup refuses with
`firmware: Echo is not booted to Android (bootmode <x>)`.
The local clock is checked before opening the hold and each audio command.
Before 09:00 the run refuses audio; there is no quiet-hours override.
`--python` selects Python 3 (default `python`) for local clock, hashing and WAV
work. Production time has no environment override.

One hold serves a batch; no alignment audio is played or saved. Commands are
pinned in `src/lab_run/run_command.rs` to the coordinator-approved CLI source.
Reply WAVs contain only post-playback samples using returned timestamps. Child
output is bounded and discarded on failure; commands have 30-second ceilings
shortened by the remaining monotonic lease budget. A hold lasts at most 11
minutes, reserving 90 seconds for playback, WAV processing and release. Rotation
releases, waits 25 seconds and reopens. The fixture clock advances without
sleeping.
After a completed playback response with a missing recording, the command gate
first waits for `playEndedAt`. Then `session status <hold> --json` must confirm the same
hold ended with reason `preempted` before the interrupted step is reopened and
retried once. A second preemption fails. Status/release JSON and argv follow
the same pinned CLI source, including `cli/internal/host/server.go`.
Unsupported steps and setup/budget/injection semantics
fail explicitly.

For cooperative cancellation, supply `--cancel-file /external/cancel.signal`.
Creating that file cancels the current child, kills and reaps it, stops later
steps and attempts one release independently of the cancellation signal. The
runner does not remove the signal file. Use a fresh path per run; an existing
signal refuses work. Both primary and cleanup failures appear in the diagnostic
and failed report. Abrupt OS termination and a remote hold that outlives a failed
release require coordinator recovery; local deadlines do not prove remote teardown.

Artifacts require a fresh directory with an existing parent, outside every git
worktree; the default is a unique host temporary directory. `events.jsonl`
records ordered, sanitized lifecycle events. `report.json` is schema-validated
and published last by rename. A reportable failure uses `result: fail` and exit
1; success uses `pass` and exit 0. The tier is the highest tier declared by the
batch scenarios; a declared tier alone does not prove that environment.
Leases, Ring identifiers, transcripts and raw child output are never written to
reports or events. `bench_session_ids` contains ordered local aliases
(`batch-hold-1`, etc.), never external hold ids or leases. Passing audio is deleted unless `--keep-audio`; failed audio
is retained outside git for triage. An interrupted or unwritable publication
may leave an incomplete external directory without a report.

`expect_link_counts` compares all eight MCP lifecycle counters: `link`,
`tools_list`, `tool_call`, `forced_refresh`, `replay_rejected`, `relink_error`,
`restart_grant_kept` and `unlink`. Each is a nonnegative integer; omitted or
unknown counters are invalid. Counts must come from driver observations.
An expected object never supplies missing measurements, even when every
expected count is zero. A mismatch prints the full expected and actual objects
and fails the step. Finalization checks the counts again after driver cleanup
so late duplicate outcomes fail the earlier assertion. Reports include optional
`link_counts` only when measurements exist, preserving actual counts on failure.
The generic drivers do not produce these measurements. The M5 runner reports
observed operator outcomes; its full eight-outcome scenario remains pending.

The schema and shared parser accept `mcp: {action: link}` with actions `link`,
`approve`, `refresh`, `restart`, `call`, `replay`, `probe` and `unlink`.
Missing, unknown or extra control fields are invalid. Both generic drivers
explicitly refuse these controls before starting a host, bridge or device
operation. Sim scenarios with milestone `M5` select the production bridge
beside `echo-lab` by default, including in `--all`; `--bridge` can override it.
Prebuild both with `cargo build --locked -p echo-lab -p echo-bridge --bins`,
and prepare the pinned host with `make hassmic-host`.

The M5 runner mounts all eight MCP controls. It owns one
private state directory outside git, the real bridge, pinned host, strict
authorization fixture, signed-token spoof and local Live fixture. Link invokes
the bridge CLI and independently checks the paired screen's code/URI/exact
expiry. Approval advances the fixture and guarded bridge clock by five seconds;
refresh and unlink use the CLI too. Restart collects only the bridge, preserves
the stored grant and key exactly, restores elapsed simulated time from the same
epoch, and checks that authorization counters did not change. The host and
external peers stay alive. Pending links and absent grants refuse restart.
Replay retains the approved grant in private memory, discovers the token endpoint,
and sends the consumed refresh with a fresh same-key DPoP proof at fixture UTC.
It requires exactly `400 {"error":"invalid_grant"}`, one new replay rejection,
resource admission changing from allowed to revoked, and an unchanged stored
grant. It refuses before rotation, while linking, after unlink or after replay;
no automatic refresh or replay retry runs. No replay credential reaches reports.
Lifecycle trace and screen transitions
supply counts, rechecked after shutdown. EOF collects the owned processes and
removes private state. Resource controls require a running linked bridge before
lazy startup. `call` wakes the pinned host and dispatches a controlled Live
client delegation through the production bridge. The local model requests one
`test.echo({"text":"m5-linked"})` and returns its result; the runner observes
both exact function output and delivered Live commentary. `probe` sends a
second delegation after replay, requiring one rejected resource request and
the exact MCP relink screen, with no extra tool dispatch. The independently
known verification URI and expiry bind the scenario's `mcp_field` operands;
the complete expected link screen is compared exactly.

After prebuilding, run `echo-lab run tests/scenarios/m5-port-os-link.yaml --driver sim`.
The report requires exactly one link, list, successful call, rejected replay,
relink transition, proven live restart and unlink, plus two forced refreshes.
Parallel isolated journeys protect the healthy outcome while a seeded count
mismatch produces a failed report. T4/T5 and physical screen proof remain
operator-owned; this T3 scenario does not declare M5 complete.

Build once with `cargo build -p echo-lab --bins --locked`. T3 tests use prebuilt
`echo-lab` and `echo-lab-fake-run`, isolated temporary directories, and per-child
`ECHO_LAB_RUN_FIXTURE` / `ECHO_LAB_RUN_LEDGER`. The latter executable implements
only synthetic port, adb and host-helper responses; it never forwards device commands.
Its JSON fixture can select `hour`, per-call `hours`, `build`, a failed operation
with `fail`, malformed JSON with `malformed`, or a response object by operation
name. Lifecycle fixtures also select per-clock-call `times` (monotonic
milliseconds, retaining the final value), per-open `holds`, a `preemptions`
count, and `cancel_on` to publish `cancel.signal` and block until killed.
`wait` acknowledges exactly 25000 virtual milliseconds; no fixture sleeps.
The fixture helper is selected explicitly with `--python`; these variables
are consumed only by fakes. Tests also exercise the real host Python helper for
PNG decoding; `real_pixels: true` forwards only the pure local pixel operation
from the fake helper to Python. The remaining operations stay synthetic.
`transcript` configures fake STT stdout (default `The time is 7:49 p.m.`).
`wait_until` advances the fixture wall clock to the parsed playback end; each
ledger entry includes `wall_ms`. `record_waits_for_frame: true` makes the fake
recording require the snapshot notification, without a fixed sleep. The 221-byte black JPEG and 2-by-2 PNG in `tests/goldens/` are
generated synthetic images, never device captures.

To reproduce B0 safely in PowerShell, build the tools once in this worktree,
then run in a fresh external directory. The synthetic speech endpoint/model
are explicit because the source-pinned CLI has no text-to-speech defaults:

```sh
repo=$(pwd)
cargo build -p echo-lab --bins --locked
fake="$repo/target/debug/echo-lab-fake-run"
run=$(mktemp -d)
cd "$run"
export ECHO_LAB_RING=fixture-ring
export ECHO_LAB_RUN_FIXTURE="$run/fixture.json"
export ECHO_LAB_RUN_LEDGER="$run/ledger.jsonl"
printf '%s\n' '{"transcript":"The time is 7:49 p.m.","record_waits_for_frame":true}' >"$ECHO_LAB_RUN_FIXTURE"
"$repo/target/debug/echo-lab" run "$repo/tests/scenarios/b0-stock-time.yaml" \
  --driver device --bench ring --port "$fake" --adb "$fake" --stt "$fake" --python "$fake" \
  --speech-endpoint fixture-speech --speech-model fixture-model --out artifacts
cat ledger.jsonl artifacts/report.json
cd "$repo"
```

Expected exit is 0 with `result: pass`. Repeat in another fresh external
directory with `transcript: "No clock information"` for exit 1, `result: fail`
and `checks.step_2: false`. The scenario declares its intended T5 milestone;
these fake executions are T3 harness evidence and never a physical B0 pass.

The real host helper is also exercised for
exact WAV samples and SHA-256. Run `cargo test -p echo-lab --all-targets --locked`.
No test accesses hardware, an account, STT service or a remote endpoint.

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

Supply an explicit Linux executable
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

The sim driver executes the W1 scenario below. Unsupported controls, touch,
frame capture, remote/Muse setup and negative injections fail explicitly.
M4 remains not runnable until the W2 assembled bridge and agent integration.

## Sim runner

Build once with `make hassmic-host` and `cargo build -p echo-lab --bins --locked`.
The host requires the development packages documented by
`scripts/hassmic-host.sh`; see [Linux hosts](../../docs/README.md#linux-hosts).
The runner never builds binaries, installs packages or accesses hardware.

```text
cargo run -p echo-lab -- run tests/scenarios/w1-wake-stream.yaml --driver sim --bridge target/debug/echo-lab-sim-bridge
cargo run -p echo-lab -- run --all --driver sim
```

`--bridge` is one Linux executable
path, including spaces, with native argv and no shell evaluation. Use a wrapper
for fixed arguments. `--python` selects Python (`python3` by default). A fresh `--cancel-file` can be created to
cancel a pending run; it
remains after cleanup. One scenario has a 120-second safety ceiling; cleanup
has its own bounded deadline and runs independently of cancellation.

Defaults write outside every git worktree under the temporary directory's
`echo-lab-artifacts/artifacts/<milestone>/<unique-run-id>/`. `--out` selects a
fresh external run directory. With `--all`, it selects a fresh external root
containing `artifacts/<milestone>/<run-id>/`. Existing destinations are immutable.
Only `report.json` marks completed publication; exit 1 and `result: fail` report
failed expectations. Preflight errors may produce no directory. A broken stdout
returns 1 even if a completed report remains. All audio and private observation
files are removed at cleanup; reports persist allowlisted Wyoming metadata,
checks and fixed diagnostic categories, never transcript or full UI payloads.

Every sim world owns the pinned host controller, the existing Spoof MCP and
FakeLive library servers, and one bridge child. Readiness uses flushed JSON
records and Wyoming registration. EOF closes the bridge and the host controller;
the controller reaps its own Linux process group and feeder/log threads.
The upstream host retains its real audio clock. Its accepted limitation appears
in every sim report verbatim:
`pinned hassmic-host binds wildcard listeners (upstream; no bind-address option)`.
Our connections and listeners use literal `127.0.0.1` only. Cleanup tests
require EOF/reset without a Wyoming response, and local fake sockets refuse
connections. T3 proves no provider, acoustic wake, hardware mute or physical
milestone behavior.

### Sim bridge contract

M1 scenarios select the prebuilt production `echo-bridge` beside `echo-lab`;
build both with `cargo build -p echo-lab -p echo-bridge --bins --locked`.
W1 retains the bundled bridge, and an explicit `--bridge` still takes precedence.
An executable named `echo-bridge` runs with an isolated configuration, synthetic
credential files, and explicit loopback Live/model overrides. Its screen is
observed through authenticated `/events` SSE. The first presentation must have
sequence 1 and message exactly `Echo Live ready`; private configuration and
pairing files are removed after the child is stopped and reaped.

The committed `sim_host` M1 prefix test currently proves Ready and one wake only.
It does not claim the complete M1 scenario, mute latch or restart counts.
Its existing FakeLive boundary sends a synthetic `response.audio.done`; this
proves Wyoming capture at the bridge, not provider behavior or a spoken reply.
Mute steps use the real producer and bridge guard. The complete M1 scenario,
reconnect generation handling, restart groups and aggregate report remain
implementation work in this lane. The production sim configuration uses the
existing zero-second session idle policy: each wake gets a fresh FakeLive call
whose script emits one response completion. Call reuse has separate bridge
component coverage.

The bridge's component recovery tests also exercise a peer closing its idle
Wyoming socket after either a streamed turn or a missing-credential refusal.
After building `echo-bridge`, run them from a POSIX shell:

```sh
cargo nextest run --locked -p echo-bridge --test bridge_t3 --run-ignored only \
  --config-file .github/nextest.toml --profile ci -E 'test(idle_socket_close)' --no-capture
```

They restore the same loopback endpoint, retain the bridge process, check
registration and health recovery, and exercise another wake. This socket-fault
proof does not replace the real-host M1 scenario or its 10+10 process restarts.

The runner replaces these variables for each child:

| Variable | Value |
| --- | --- |
| `ECHO_LAB_WYOMING_ADDR` | `127.0.0.1:<host-port>` |
| `ECHO_LAB_MCP_URL` | `http://127.0.0.1:<spoof-port>/mcp` |
| `ECHO_LAB_LIVE_URL` | `http://127.0.0.1:<live-port>/` |
| `ECHO_LAB_TRACE_PATH` | Absolute native private JSONL path |
| `ECHO_LAB_UI_STATE_PATH` | Absolute native private UiState JSON path |

After registration, the bridge flushes `{"event":"bridge.ready"}` on stdout.
After a complete wake it flushes
`{"event":"bridge.run_completed","run_id":"run-1"}`. Closing stdin requests
shutdown; exit 0 closes the trace, and nonzero exit fails the run. These are
local harness records, separate from production bridge configuration.
`echo-lab-sim-bridge` counts actual microphone frames through echo-wyoming,
plays a short silent reply and records start, frame count and end. It never
invents the microphone `audio-start` absent from the pinned host.

Trace records are UTF-8 JSON objects, newline terminated, with `event` and
nonnegative integer `elapsed_ms`. Run observations require `run_id`: 1–64 ASCII
letters, digits, hyphens or underscores. Timestamps cannot regress within a run.
The private trace is bounded to 1 MiB. Partial lines wait until complete and
fail if still incomplete at shutdown. Complete malformed lines fail.

### Sim steps and oracles

`say` selects scenario-relative raw s16le mono 16000 Hz `.pcm` data (1–2048
samples), cycled through the host capture FIFO before SIGUSR1. The W1 fixture
is a durable synthetic 400 Hz waveform, 320 samples at amplitude 2000, generated
as `int(2000 * sin(2*pi*i/40))`; it contains no recorded voice.
`press` accepts `wake`/SIGUSR1, `action`/SIGUSR2, `pair`/SIGWINCH, `stop`/SIGHUP and
`mute`, either as a string or `{key: action}`. Mute sends native Linux KEY_MUTE
down/up events through the pinned host's private `-b` keypad FIFO and waits for
its button-state log before acknowledging. The real Wyoming mute event remains
the observer's state barrier; a control acknowledgment alone proves no bridge
privacy or screen state. Other controls fail.

`expect_trace` is a nonempty ordered array of selectors: `EVENT {scalar: value}`.
For example `wyoming.mic_frames {count_gte: 10}` checks the numeric frame payload;
`tools/call {name: test.echo, count: 1}` requires exactly one matching record.
`count` defaults to one occurrence. Matching remains within the current run;
unrelated events may intervene. Finalization checks exact counts again, so a
late duplicate fails its earlier step. Unknown operators and invalid types fail.
`mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}`
checks a bridge measurement independent of the previous wake's run ID. Both
explicit counts must be zero; missing, stale or duplicate windows fail. A seeded
forwarded count of one fails with `forwarded_frames: expected 0, observed 1`.
A `say` after observed Muted waits for native capture consumption without
waiting for a nonexistent run. Only the bridge measurement, emitted on unmute,
proves zero microphone traffic.

`expect_screen: {state: speaking}` compares selected fields against the latest
complete typed `echo_screen_contract::UiState`. Wrong, missing or malformed
state fails. `expect_speech_contains` consumes the current run's latest
`live.transcript` record's `text`, using the existing case-insensitive literal
matcher and explicit `regex:` opt-in. Its text never enters reports.
`first_audio_after_endpoint_ms_p95` sorts current-run `timing` records named
`first_audio_after_endpoint_ms`, selecting nearest rank `ceil(0.95*n)` of their
nonnegative `duration_ms`. Equality at the budget passes; missing/invalid samples
and unsupported metrics fail. W1's bridge produces neither speech, UI nor timing;
scripted T3 bridges test those observations without claiming W2 integration.
The production voice bridge writes that `timing` record after a settled reply's
`reply.quality`: `duration_ms` runs on the bridge clock from the endpoint to the
first reply frame handed to the Echo. The endpoint is the Live session's first
sign that the user's turn is over, named in `endpoint`: `user_transcript`, or
`delegated_turn` when the delegated turn starts first. A reply whose audio starts
before either sign has no sample. The M4 golden comparison skips `timing`, as it
skips `reply.quality`: the value follows the run's pace.

`make test-sim` runs two halves. Both build the
lab binaries once. `make test-sim-suites` validates scenarios, runs the
host/Wyoming component tests and discovers all T3 scenarios. CI runs both halves
as parallel part jobs. `run --all --partition K/N` runs every Nth T3 scenario in
sorted order, starting at the Kth, so partitions 1/N to N/N run each scenario once.
`make test-sim-host` runs the ignored `sim_host` integration cases and the
binary's ignored T3 cases, each with two workers. Discovery prints pass/fail/not-runnable
outcomes and `executed`, `passed`, `failed`, `not_runnable` totals. A runnable
failure or zero executed scenarios returns 1; M4 is never counted as passed.
The test-only `echo-lab-sim-script` binary consumes explicit synthetic scripts
at the child-process boundary; it is never the W1 acceptance bridge.

M1 uses the production bridge by default:

```sh
make hassmic-host
cargo build --locked -p echo-lab -p echo-bridge --bins
cargo run --locked -p echo-lab -- run tests/scenarios/m1-hassmic-bridge.yaml --driver sim
```

The schema-valid report's `m1` measurements include one Ready presentation before the first wake,
all 22 streamed wakes (each at least 10 microphone frames), the real native mute window
(received and forwarded both zero), and 10 bridge plus 10 hassmic-host restarts. Run IDs are
scoped by `bridge_generation`. Each restart stops and collects that process before replacement,
checks the opposite process identity, reconnects on the same host endpoint, and runs the ordered
wake oracle. The scenario has a 120 second ceiling; no real service, device or account is used.
If the native latch acknowledges but the real screen stream does not show Muted within two
seconds, the runner records the mute observation as unavailable. It skips only the dependent
privacy checks, unlatches the host, and still runs the next wake and both full restart groups.
The report returns `blocked`, omits `mute_window`, retains measured wake/restart counts, and
the command exits 1. This does not diagnose the missing producer or prove privacy. Any other
failed check, malformed screen event, faulty mute measurement or cleanup failure returns `fail`.
Component coverage for one independent restart of each process is available with:

```sh
cargo nextest run --locked -p echo-lab --test sim_host --run-ignored only \
  -E 'test(m1_independent_restart_steps)' --config-file .github/nextest.toml --profile ci --no-capture
```

### M1 device procedure (coordinator only)

Run the unchanged `tests/scenarios/m1-hassmic-bridge.yaml` after the simulated
journey passes. The device driver refuses audio before **09:00 local time**,
including each later playback; this is not a UTC gate.

Forward Wyoming from the connected Echo to the bridge host:

```sh
adb forward tcp:16700 tcp:16700
```

Copy `examples/bridge.example.toml` outside the repository. In its existing
`[echo]` table set `host = "127.0.0.1"` and `wyoming_port = 16700`.
The nominated configuration must use loopback screen and unauthenticated local
MCP endpoints; it must not nominate real auth state or Muse credentials. Never
include the operator's Codex home: any `codex.home` key is refused with
`device: invalid or unsafe nominated bridge configuration`, even if its path
does not exist. Remove `home` from the disposable nomination before retrying;
leave the operator home and its `auth.json` untouched. Home-free `[codex]`
settings may remain. The
device journey validates it before effects and stages a private synthetic
configuration. A LAN Echo address is refused with
`device: echo.host must be 127.0.0.1; forward the Echo's Wyoming port with adb forward`.

Supply an operator WAV outside every repository worktree: PCM format 1,
16-bit mono, 16000 or 48000 Hz, 0.5–10 seconds inclusive, containing the spoken
wake word **Computer** and the intended utterance. No audio is committed.

```sh
target/debug/echo-lab run tests/scenarios/m1-hassmic-bridge.yaml \
  --driver device --bench ring --port /path/to/port --adb /path/to/adb \
  --stt /path/to/stt --ring "$RING_ENDPOINT" \
  --bridge /path/to/echo-bridge --bridge-config /outside/repository/bridge.toml \
  --wake-clip /outside/repository/computer.wav \
  --operator-confirm /outside/repository/mute-confirm \
  --out /outside/repository/fresh-m1-run
```

Watch stderr. At the first mute prompt, physically press the Echo's mute key,
then create the nominated confirmation file. After the muted playback, at the
second prompt, physically press mute again to unmute, then create a **fresh**
confirmation file at that path. Each confirmation expires after 120 seconds
and is consumed once; a stale file cannot confirm a later press. File creation
does not replace the physical press or the observed Muted/Ready screen state.

Every plain, muted and restart `say` uses the same validated snapshot. The
optional `m1.wake_clip` records the complete WAV SHA-256, rate and duration;
`m1.plays` records each successful playback, and streamed wakes reference its
`play_index` and source. Reports contain neither audio nor the operator path.
Run-owned clip copies are removed even with `--keep-audio`; the original WAV
remains caller-owned. Omitting `--wake-clip` retains fixture playback and the
original report shape. Sim mode rejects the option. T3 evidence proves source
routing only; the coordinator's post-merge T5 must prove acoustic wake, physical
mute and recovery before M1 can be declared complete.

### Bridge on the Echo (coordinator only)

W4 runs the bridge on the Echo, beside `hassmic`, with no PC in the voice path.
`--device-bridge <launcher.json>` replaces `--bridge`: the driver starts no host
bridge and no wire relay. It needs `--bridge-config` and `--live real`. For each
run and each `restart: {process: bridge}` step the driver:

1. derives the scenario's effective configuration, writes it to the run's
   private directory, pushes that file (never the operator's) to the launcher's
   `config` path, sets mode 0600 and reads the mode back;
2. stops any bridge the launcher started, removes the old trace and starts the
   bridge through the launcher;
3. waits for `bridge.ready` in the trace, read back with `adb exec-out`;
4. waits until the hassmic log written since the start shows `client connected`
   or `satellite: running` with no later `client disconnected`;
5. reaches the screen stream through its own `adb forward tcp:0`, removed at close.

Liveness is the process name in `adb shell ps -A -o PID,NAME`. Before each wake
the hassmic log must still show the link. A wake that hassmic answers with
`wake word: no Home Assistant link, nothing started` fails with
`device: wake without a hassmic link`. The wake clip still plays through the Ring.

Under `--live real` each reply, screen and trace observation waits 20 s plus twice
the effective `voice.session_listen_seconds` (at most `voice.session_max_seconds`),
50 s by default, because the bridge keeps the run open for a follow-up after its
reply; fake Live keeps 20 s, and so does bridge startup.

The launcher is a JSON file kept outside the repository. Every device path comes
from it; the driver holds none.

```json
{
  "private_dir": "/data/local/echo-bridge/private",
  "config": "/data/local/echo-bridge/private/bridge.toml",
  "trace": "/data/local/echo-bridge/private/trace.jsonl",
  "hassmic_log": "/data/local/hassmic/boot.log",
  "binary": "/data/local/echo-bridge/bin/echo-bridge",
  "run_as": "/data/local/hassmic/runas nobody 3990,inet"
}
```

| Key | Meaning |
| --- | --- |
| `private_dir` | The bridge's one 0700 directory. `config` and `trace` must be inside it. |
| `hassmic_log` | The log `hassmic` writes its link and wake lines to. |
| `binary` | The armv7 `echo-bridge`. Selects the built-in start and stop commands. |
| `run_as` | Omitted/empty defaults to the absolute nobody prefix above. Short `runas nobody 3990,inet` normalizes to it. Root/uid 0 is refused before effects. |
| `process` | Optional process name to look for. Default `echo-bridge`. |
| `start`, `stop` | Instead of `binary`: exactly `["shell", "PAYLOAD"]`, executed under nobody. Scripts must be executable by nobody; payloads cannot contain single quotes. Other argv forms are refused. |

A root launcher is refused with `device: on-device launcher must run as nobody, not root`.
The private directory must already be owned by 9999:9999, mode 0700. Root adb pushes the
run-specific config, transfers it with `chown 9999:9999`, then nobody chmods/stats it;
readback must be exactly `9999:9999:600`. Launch, trace reads, stop and cleanup use nobody.

The built-in start follows the launch contract in `docs/architecture.md`: it
runs under `setsid`, with stdin held open by a FIFO writer, and the built-in
stop ends that writer so the bridge sees end of input. `start` and `stop`
templates may use `{private_dir}`, `{config}` and `{trace}`. Paths accept only
letters, digits, `_`, `.`, `-` and `/`; anything else is refused with
`device: invalid on-device launcher`.

The nominated configuration is the one the Echo runs: `echo.host = "127.0.0.1"`,
device paths, and a screen pairing file holding the lab's synthetic pairing
value. Stop any bridge started by another script first; two bridges fail the run.

```sh
target/debug/echo-lab run tests/scenarios/m1-hassmic-bridge.yaml \
  --driver device --bench ring --live real --port /path/to/port --adb /path/to/adb \
  --stt /path/to/stt --ring "$RING_ENDPOINT" \
  --device-bridge /outside/repository/launcher.json \
  --bridge-config /outside/repository/bridge.echo.toml \
  --wake-clip /outside/repository/computer.wav \
  --operator-confirm /outside/repository/mute-confirm \
  --out /outside/repository/fresh-m1-on-device

target/debug/echo-lab run tests/scenarios/m4-spoof-tool.yaml \
  --driver device --bench ring --live real --port /path/to/port --adb /path/to/adb \
  --stt /path/to/stt --ring "$RING_ENDPOINT" \
  --device-bridge /outside/repository/launcher.json \
  --bridge-config /outside/repository/bridge.echo.toml \
  --wake-clip /outside/repository/computer-test-tool.wav \
  --out /outside/repository/fresh-m4-on-device
```

Differences from the host-bridge mode:

- M4's spoof MCP still listens on the bench host; the driver adds
  `adb reverse tcp:<port> tcp:<port>` for the configured port and removes it at close.
- `first_audio_after_endpoint_ms_p95` is measured from the trace's `timing`
  records, with the host mode's percentile rule and verdict text. A trace with
  no such record stops at `device: unmet prerequisite: first_audio_after_endpoint_ms
  timing sample in the bridge trace from the Echo`. The bridge's endpoint is a
  Live session event, later than the end of speech the host mode matches on the
  wire, so the two modes' numbers are not interchangeable.
- `restart: {process: hassmic-host}` ends the link, and a bridge on the Echo ends
  with it. The driver starts it again and does not count that exit's
  `bridge.error` records against the run.
- `m3-full-duplex` and the `session-*` scenarios are judged on host wire
  receipts and are refused by name.

M6 extends the M4 production world with the spoof `ui.show_emotion` tool:

```sh
target/debug/echo-lab run tests/scenarios/m6-emotion.yaml --driver sim
```

The paired SSE observer checks Ready, Listening, Emotion, Listening, Speaking,
and Ready by exact value with strictly increasing sequence numbers. The happy
animation has frames `(^_^)` and `(^o^)` at 80 ms. Only after observing Emotion
does the harness advance the guarded bridge clock by 160 ms. The controlled
Responses peer holds terminal output until the native return to Listening is
observed. The report's optional `m6` measurement retains the received screen
frames, duration, return state, forwarded reply bytes and host playback counts.
A still image fails with `m6: animation frame count 1; expected at least 2 distinct frames`.
The later reply must forward 9600 PCM bytes, with the same host playback count
and zero nonzero bytes. This proves synthetic playback through the host; real
speech, device animation and program milestone M6 require the operator T5 run.
