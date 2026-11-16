---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: accepted
---

# Protocol-neutral Echo link

The owner directed the bridge away from Home Assistant protocols wherever
possible (2026-11-15, [decisions](../program/decisions.md)). `echo-link` provides
the device operations the existing bridge actually uses: capture, wake metadata,
run-scoped PCM and statistics, probe, bounded reply production, acknowledgment,
cut, pause, close and connection establishment. The adapter retains reconnect
ownership. Error categories distinguish timeout, transport and malformed protocol
without copying peer data into diagnostics.

The coordinator's binding PCM decision C (2026-11-16) places `PcmFormat` in
`echo-link`: rate, bytes per sample and channels, with no extension map. The
Wyoming adapter converts at its boundary and defaults outbound extensions. The
bridge converts privately when calling the unchanged `echo-audio` API. Neither
`echo-audio` nor `echo-contracts` changes in this refactor.

The crate and its optional in-memory fake use only `std`. Send futures express
the existing asynchronous boundary without choosing an executor. The Wyoming
adapter wraps its current bounded sender and directly consumes its current
receiver; there is no new pump, queue, reconnect policy or wire vocabulary.
Only the binary composition selects that adapter. A private Live port lets the
actual bridge journey run with an in-memory observer, without a provider call,
Wyoming implementation or socket. The trace clock uses the runtime's monotonic
clock so the paused-clock witness asserts complete records deterministically;
production event vocabulary, counters and timing meaning are unchanged.

The current target remains local Wyoming, including the planned W4 move onto
the Echo after M1. A future generic local transport can implement the same seam
as a second adapter. This decision neither selects nor implements that transport
and authorizes no device change or credential access. The existing Wyoming
client API, fixtures, sim oracles and diagnostic strings stay compatible.

Rollback removes the neutral seam and returns binary wiring and library callers
to the unchanged Wyoming client. No stored state or wire migration is needed.

## Amendment: device mute

The coordinator authorized this additive contract on 2026-11-16 for M1:

1. `LinkEvent::Mute(bool)` is the effective device mute (privacy latch or soft
   mute), delivered in wire order with `Wake`, `Played` and `Other`.
2. A connection generation is a link instance and starts unmuted. If already
   muted, its first event is `Mute(true)`. Absence of a snapshot means unmuted.
   The Wyoming adapter resets effective mute at each recovered socket generation.
3. Device mute ends the microphone at the mute boundary. Preceding frames drain,
   `MicrophoneRun::recv` returns `Ok(None)`, and the matching `Mute(true)` follows
   on `LinkEvents`. Frames sent after mute never reach the microphone consumer
   and increment `RunStats::dropped`.
4. `Other` stays payload-free, with no Info identity. The adapter retains the
   connection-time Info barrier; the bridge consumes only neutral events.
5. `LinkEvent::MicrophoneDropped(u64)` reports suppressed microphone chunks in
   wire order without PCM or format data. It includes idle strays and chunks
   after mute terminates a run. Counts may be coalesced only without crossing
   another event; their sum is the exact suppressed chunk count per connection.

The bridge counts yielded `MicFrame`s once as received and counts suppressed
chunks once through `MicrophoneDropped`. While muted these contribute to the
mute window's received total; neither path forwards audio while muted.
`RunStats::received` and `RunStats::dropped` remain run-scoped transport
statistics (including active-run post-mute discards). They are diagnostic
fields in `mic.completed`, never added to the bridge's received total, so a
suppressed active-run chunk is not counted twice.

The in-memory fake scripts a muted connection snapshot or a mute at a specified
frame position. This amendment adds no dependency, provider access or device run.

## Amendment: unserved runs

The coordinator authorized `RunFailure` and `EchoLink::fail_run` on 2026-11-16.
The closed causes are `RelinkRequired`, `KillSwitch` and `Unavailable`. Only the
Wyoming adapter maps them to `echo-relink-required` / `Relink required.`,
`echo-kill-switch` / `Kill switch enabled.`, and `echo-unavailable` /
`Live unavailable.`. The existing session writer sends one Wyoming error and
ends local microphone routing without pausing registration or closing the
connection. Trailing microphone frames are discarded; the next wake remains
enabled. No synthesized speech or peer diagnostic enters this boundary.
Rollback reverts the capability, callers and fakes together; no migration is
needed and the previous pipeline-timeout behavior returns.
