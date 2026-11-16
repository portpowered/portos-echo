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
