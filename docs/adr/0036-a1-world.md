---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: accepted for A1
---

# ADR 0036 A1 World composition

The bridge session controller constructs the World and injects its device port
into Wyoming. The World contains the concrete subsystem state, clock and RNG.
Each device event steps the reducer synchronously and can publish one view.
Clocks report monotonic duration and wall time. The bridge uses Tokio monotonic
time and its corrected model clock. DPoP and the other three A1 wall sites use
the host clock in their existing units. The Codex nonce named in the plan
belongs to the supervisor's home-lease regression test.

Wyoming converts wire observations into device events and applies the published
view before admitting audio. Sockets, bounded PCM queues, registration replay
bytes and playback acknowledgements stay in the service. The capture queue's
mute barrier drains previously admitted PCM and blocks subsequent reads before
notifying the voice owner. The ordered event stream publishes speaker reports from the
view and resets the projection at generation boundaries. The standalone
Wyoming API uses the shared transitions in `echo-core-api` with private state.
The subsystem exposes its capture and speaker invariants for verification. The
device state rejects stale generation observations.

`echo-core-api` uses the existing locked `rand_core` OS entropy source. The
deterministic fixture uses SplitMix64 and an explicitly advanced clock.
`echo-sub-device` depends only on the core API. The transport service consumes
that same API, and the session controller supplies the concrete subsystem.

The World steps the device reducer before the trace reducer. `echo-sub-trace`
admits prepared descriptors to a fixed-capacity FIFO. `echo-svc-trace` owns
the append handle, JSONL encoding, flush and join. Each writer creates files
with mode 0600 and reopens on the existing generation boundary. Synchronous
audit callers receive a result only after each row is flushed. Periodic clock
failure diagnostics can wait for the 100 ms trace tick because they carry no completion
receipt. A checked write can wake the writer earlier. The session World counts
rows until the writer sends a flush or failure event through the ordered step.
The clock monitor and voice share one writer and keep their respective elapsed-time
origins. Close stops the clock monitor, then stops admission and joins the drain.
Replacement receipts retain their separate open lifetime after teardown.

The T3 harness gives each bridge child a fresh owner-only directory and reads
its World result after collecting the child. The controller remembers the
first failed step invariant and checks the final device and trace state
after cleanup. W1 uses the same controller with the raw Wyoming client and
observes the result of its existing synchronous trace writer. The harness
prints one World status per scenario. A failed scenario prints one redacted
diff and preserves the existing journey error if there is one.

To compare deterministic snapshots, prebuild with `make sim-prereqs`.
Run this command twice with different output directories:

```sh
ECHO_LAB_WORLD_SEED=101 target/debug/echo-lab run tests/scenarios/m2-live-connect.yaml --driver sim --out <fresh-dir>
```

The M2 fixture uses its paused Tokio clock, with a fixed World epoch and seeded
RNG for these invocations. Compare the two `world-snapshots.private.json` files
with `cmp`. The files contain the three final results in scenario case order.
Each contains only typed device state and trace drain status. Ordinary
invocations remove the per-generation files on completion.

The prebuilt M2 milestone CLI test sets `ECHO_LAB_TRACE_TICK=1` for the success
journey. The guarded paused bridge queues one clock diagnostic while the writer
is held. After the normal reply and idle return, the harness releases the writer
without waking it, advances exactly 100 ms and reads one complete, byte-exact
row while the device stays idle. The playback, provider and credential oracles
still run.

## A2 reply pacing

The controller steps device, reply pacer and trace in that order. The pacer
owns the 128 fixed 960-byte slots, one staged frame, the audio clock, jitter
history, microphone measurement, mouth timing and lip batches. Its Copy view
reports retained depth, playback activity and the last hand-off time.

The voice service resamples PCM and recovers the same boxed input scratch
after each synchronous event. It releases a staged frame to the existing
transport, then reports that hand-off to the pacer. A cut clears the ring and
staged frame before the existing device flush. Playback completion requires
the accepted device receipt. Run opening seeds the measured jitter cushion
for the first peer arrival in the native World. Standalone component fixtures
start with an unprimed World for scripted arrival tests. The voice loop wakes
separately for PCM offers and the World's mouth tick. The trace writer has its
own deadline. The screen adapter publishes each lip batch through the existing
sender and reports whether it was taken.
`PeerFrameDiscarded` restarts the arrival chain and keeps the learned jitter.
`RemoteEnd` drops queued and staged PCM before the device flush. Ordinary
reply settlement still handles its pending lip samples. Barge-in uses the
cut event to publish an immediate closing sample. `LipStatsCollected` clears
the counters after the screen delivery receipt and the service's stats read.
The binding mailbox answer for `sb-a2-pacer-subsystem` limited the new variants
to these three inputs. [Coordinator ruling](https://github.com/portpowered/portos-echo/pull/357#issuecomment-7091032723)

`echo-sub-pacer` depends on `echo-core-api` and the existing `echo-audio` DSP.

## A2 session lifecycle

The controller steps device, session, pacer and trace in that order. It
allocates one voice run slot and passes a generational handle to the reducer.
Release invalidates the handle after the run's cleanup. A reused slot rejects
events from the previous generation. The slot implementation retires entries
whose generation is exhausted.

The pure session reducer owns response, run-ceiling and quiet deadlines,
playback receipt ownership, and capture settlement. The bridge passes
monotonic offsets from its existing clock, including under paused simulation.
A settled reply arms the quiet deadline. A close issues one end request
after playback and mouth settlement, and capture EOF confirms completion.
The service awaits existing device writes, playback, and capture at their
original boundaries. It also consumes the call open and close commands at
the existing call setup and teardown boundaries.

## Deferred

- The device step receives wake and mute events. Wyoming has no separate
  button report in the current wire contract.
- Within this lane, the next two stories move run grants and tool slots,
  then barge, floor and the remaining speech and hand-back policy.
- A2 owns the Wyoming service rename, call reducer and phase-wide allocation
  proof. The call reducer steps after pacer and before trace when introduced.
  The retained-call policy and native goodbye path keep their current rules.
- Playback join, reply completion and delegated turn completion retain their
  service fences. The global inbox, record/replay, modeled speaker clock
  and full allocation proofs remain with their existing plan gates.
- Generational tool slots apply to session-control requests. The general
  dispatcher, pending RPCs and tool ledger remain in B1.
- The bounded EchoLink/Wyoming queue and serialization still use owned
  transport buffers. Their allocation work belongs at the service boundary.
- The stepped simulator keeps its current adaptive-hold bypass. The
  simulated speaker clock and modeled tail remain later work, with the
  new timing and lip goldens in C1.
- A2 and B2 own migration of existing direct randomness consumers. The later
  service tracks own record and replay.
- Trace rotation policy remains with later lifecycle work.
