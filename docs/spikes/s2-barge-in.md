---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: measured on pinned host; physical cancellation remains an M3 gate
---

# S2: Wyoming reply cancellation

Recommend **early `audio-stop`, with a bounded bridge send queue**. On the pinned
host this drains queued audio; it does not discard it. Neither `pause-satellite`
nor closing the connection closes playback in the observed window. An immediate
discard operation would need an upstream change, with separate authorization;
this spike changes neither upstream nor the bridge's shared contracts.

## Measurements

Measured in WSL Ubuntu against `hassmic.pin` commit
`8646a4f2f4aeafde2d7aca7359d9eff555a169c0`, using the existing prebuilt host
with `-P wyoming -D`, synthetic microphone silence and loopback sockets only.
The first full `make measure-barge-in` run produced:

| Strategy | Trials | Median cut to sink close (ms) | Maximum (ms) | Result at cut + 4000 ms |
| --- | ---: | ---: | ---: | --- |
| early audio-stop | 10 | 2004.263 | 2005.373 | 144000 bytes, one played, sink closed |
| pause-satellite | 10 | Not estimable | Not estimable | 143360 visible bytes, no played, sink open |
| close-stream | 10 | Not estimable | Not estimable | 143360 visible bytes, no connection, sink open |
| none, normal completion | 10 | 2004.141 | 2004.545 | 144000 bytes, one played, sink closed |

For pause and disconnect all ten observations are right-censored at 4000 ms:
there is no finite playback-close latency to aggregate. This reports the
packet's F2/F3 no-acknowledgment outcome rather than inventing a stopping time.
The table's missing estimates are not zero latency or proof of infinite playback.
Raw per-trial cut offsets and endpoint observations belong in the PR comment.
These shared-host timings are observations, not new acceptance thresholds.

Each trial queues thirty 4800-byte chunks of 24 kHz, signed 16-bit little-endian
mono audio: 144000 bytes, exactly three seconds. A FIFO at `HASSMIC_PLAY` records
the actual output bytes. Its first readable bytes define the observed playback
start; its writer EOF independently timestamps playback closure, including when
the Wyoming connection is gone. The cut action occurs one second after that
observed start using monotonic time. Readiness comes from sockets, logs, FIFO
readability and capture consumption; elapsed time is used for this requested
measurement and bounded observation windows.

The host's stdio buffering means first readable bytes are not an acoustic start.
An open sink retains 640 unflushed bytes after the 143360 bytes visible to its
reader; that visible prefix is checked byte for byte. The host's completed
`play_write` sleeps and a physical speaker endpoint cannot be inferred from the
last visible byte. Only FIFO EOF is counted as playback end. Thus pause and
connection close are failures to terminate the output stream, with no claim
about a physical speaker continuing to make sound for four seconds.

The no-action control takes no cancellation action at its one-second marker.
It sends normal `audio-stop` at three seconds from observed start. Other than
early audio-stop, no cleanup stop, resume or reconnect occurs during the measured
four-second window. Cleanup stops for pause/disconnect happen afterward.

## Wire and wake evidence

Fixtures use the existing normalized `{dir,header,payload_sha256,
payload_prefix_b64}` envelope. Timestamps become zero; audio is represented only
by length, digest and a 32-byte prefix. All incoming rows are received from the
real host; no microphone audio-start is inserted. The capture retains the first
microphone chunk per observation phase and consumes/checks intervening chunks;
it is a bounded event window, not a full timed transcript. Pings used as barriers
are omitted. Wakes are real SIGUSR1 host controls, not Wyoming events.

Each trial sends one wake at observed playback start + 0.5 seconds and waits for
the host's `left to the client (-D)` log. A second wake follows the cut action
immediately, before recovery. For connected speaking states the same log proves
processing. For pause/disconnect, a capture-consumption barrier proves the
pending signal was handled, then the bounded window observes no new pipeline.
The CLI separately proves a fresh run after resume/reconnect.

- [Early audio-stop fixture](../../tests/fixtures/wyoming/barge-in-audio-stop.jsonl):
  lines 4–34 are the reply; lines 35–36 retain microphone data around the first
  wake; line 37 is the cut, line 38 continued microphone data after the second
  wake, line 39 the sole played. Lines 40–42 prove a fresh run.
- [Pause fixture](../../tests/fixtures/wyoming/barge-in-pause-satellite.jsonl):
  line 37 pauses; no new pipeline or played arrives during the window. Lines
  38–39 are the subsequent cleanup stop/played; lines 40–42 resume a fresh run.
- [Disconnect fixture](../../tests/fixtures/wyoming/barge-in-close-stream.jsonl):
  the socket and its file wrapper close after line 36. There is no wire event
  for closing a socket. Lines 37–38 are cleanup stop/played on a new connection;
  lines 39–41 prove the new run. The old connection cannot receive played.
- [Control fixture](../../tests/fixtures/wyoming/barge-in-none.jsonl):
  line 37 retains microphone data after the second wake, line 38 is the normal
  completion stop, line 39 played, and lines 40–42 the fresh run.

No new run-pipeline is emitted by either speaking wake. Under `-D` the existing
microphone stream continues. Pausing suppresses that stream; disconnect removes
its transport. None of these wake observations is a physical Pryon/AEC result.

## Reproduce and regress

~~~sh
make measure-barge-in
make measure-barge-in
git diff --exit-code tests/fixtures/wyoming
make test-sim
make ci
~~~

The Make target builds/caches the exact pin using the existing host builder.
Tests consume the prebuilt artifact; they never compile it. Windows callers use
the unchanged WSL dispatch. Setup prerequisites remain build-essential,
pkg-config, libopus-dev and zlib1g-dev in WSL/Ubuntu.

`python3 scripts/measure-barge-in.py --output-dir DIRECTORY` publishes to an
independent destination. Same-output writers fail explicitly on an existing
lock. Publication stages all results, preserves other fixtures and rolls back
failed replacement. Cancellation reaps only the owned host process group and
removes temporary state and lock. Never run `make capture-wyoming` after adding
S2 fixtures: that older command replaces the whole directory with its five files.

`--unit-test` checks injected-clock arithmetic, median/max, close ownership,
normalization/digests, cancellation/failure preservation, competing writers and
failed replacement rollback without sockets. `--self-test` checks the five
original fixtures through their CLI, injects real-host timeout/exit/cancellation
through CLI dispatch, then runs one repetition of every strategy and both wake
windows. It asserts the fixed drain/open outcomes, exact playback prefix/full
bytes, played count, no extra pipeline, and byte-identical normalized fixtures.
It publishes nothing. Full measurement also rejects differing trial windows.

This is the source-plan section 3.1 S2 host-measurement exception described in
the packet: the existing shell/CLI boundary supplies real elapsed-time evidence;
`echo-lab` has no run/sim driver on this base. Pure timing arithmetic is offline.
No new scenario driver or M3 milestone proof is claimed.

## Remaining gates

Review owns S2-P2 (I-1/R-CI), S2-P3 (R-MERGE), S2-CI
(R-CI/R-RUNTIME/R-MERGE), and S2-LOOPBACK (V-1): independent clean-head reruns,
terminal hosted CI, integrated clean-room validation and merge. Actual Live
cancellation, physical audio latency, Pryon and AEC remain M3-T3/M3-T5 work for
the coordinator. This host-only result does not authorize a real account or
hardware run. No independent story is deferred from this slice.
