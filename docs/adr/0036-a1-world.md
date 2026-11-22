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
deterministic fixture uses SplitMix64 and an explicitly advanced clock. The
device subsystem depends only on the core API. The transport service consumes
that same API, and the session controller supplies the concrete subsystem.

## Deferred

- A1's next slices move trace IO to its service and join the World to the T3
  scenario finalizer. The device step currently receives wake and mute events.
  Wyoming has no separate button report in the current wire contract.
- A2 owns the Wyoming service rename, session, pacer and call subsystems,
  playback clock, slot table and allocation proof.
- A2 and B2 own migration of existing direct randomness consumers. The later
  service tracks own record and replay.
