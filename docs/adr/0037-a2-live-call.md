---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: accepted for A2
---

# ADR 0037 A2 live call subsystem

`echo-sub-call` depends on `echo-core-api`. The controller steps device,
session, pacer, call and trace in that order. World owns the phase, diagnostic
setup stage and receive generation for one attached call at a time. Every
observation after attachment carries the call handle. A replacement peer
receives a new handle, so an old receipt cannot change the current call.

The bridge keeps the Codex signaling and WebRTC peer behind the existing
service awaits. The matching answer and peer readiness share the original
deadline. The service applies open and close effects at its existing setup and
teardown fences and acknowledges close after peer cleanup.

The replacement assistant transcript remains queued across a cancelled receive
reset. Its generation advances before the reset, and that generation becomes
current when the same peer acknowledges it. The voice adapter still receives
late retired audio for its existing discard accounting. A complete frame is
passed inline as 320 samples. The private World report uses its existing
schema and accepts the fixed diagnostic for a closed call with a pending reset.

World allocates 1,500 inline PCM slots for the uplink ring at construction.
The current session handle owns the backlog. An offer keeps its head until
the peer's non-blocking send accepts it. Full and failed sends leave the head
pending. A matching run and offer are required for each receipt. Overflow
evicts the oldest unsent frame. Mute and capture release clear unsent speech,
including when the peer stays open for the next wake.

The coordinator's binding answer for `sb-a2-call-subsystem` on 2026-11-23
approved the internal Rust API at `16735684`. `CallEvent::Uplink` carries the
run and its `UplinkEvent`. Send commands and receipts correlate the attempt
so a receipt from before mute cannot remove a new frame in the same run.
The command slot holds one action and one frame and reports an eviction for
the step. The view exposes the backlog's current run.

The same answer approved `allocation-counter` as the workspace's test-only
allocation counter. `echo-sub-call` declares it as `allocation-counter = "0.8"`
with the resolved version in `Cargo.lock`. Its allocator is linked into the
reducer's unit-test binary. The test counts total allocations on the current
thread while filling, overflowing and draining the ring after construction.
`sb-a2-zero-alloc` will reuse this counter for the broader per-frame bench.

Capture decodes into reused sample storage and collects complete frames in
reused output storage. Invalid chunks consume no converter input and the
incomplete tail stays unsent. Mute discards the converter with its filter
history as before.

## Deferred

- The separate WebRTC transport queue retains its capacity and notifications.
- The reply pacer's microphone meter reuses its existing event buffer.
- The global inbox, deterministic replay, Opus packet buffer reuse and whole-core allocation proof
  remain with their existing plan gates.
