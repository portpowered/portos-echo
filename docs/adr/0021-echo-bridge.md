---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# ADR 0021: Bridge audio orchestration

`echo-bridge` owns the Wyoming to Live journey, using the existing `echo-audio`,
`echo-config`, `echo-contracts`, `echo-live` and `echo-wyoming` public APIs.
It reuses the locked `tokio`, `serde_json` and `thiserror` dependencies;
tests reuse `tempfile`. No external package is introduced.

One multi-thread Tokio runtime drives one Live call owner. Conversion uses
fixed scratch and complete 20 ms Opus frames. Reply buffering is bounded and
evicts oldest frames. The local health listener accepts one bounded request
at a time, with a deadline. Trace records contain fixed categories and counters.

Lab mode validates all three existing lab inputs before dialing and uses only
synthetic credentials. The echo adapter completes after ten microphone chunks
have been converted, all packets echoed and their count acknowledged. This
adapter does not establish real provider completion vocabulary. Unknown events
never imply success. The coordinator's M2 gate owns real provider compatibility.

Configured API-key mode reads only its nominated key file. ChatGPT mode reports
authorization unavailable until the separately owned sign-in integration mounts
its provider. No home-directory credential discovery is used.

The optional `[audio]` table sets `reply_lead_ms`, default 200, as a positive
multiple of 20 that fits `u16`. Existing configurations keep the default. The
bridge sends at most ten initial frames at that default, then uses echo-audio's
20 ms Pacer without catch-up bursts. Underflow does not replenish initial credit.

The provisional `response.done` event with `response.status = "cancelled"`
stops production. The bridge clears its complete backlog and calls Wyoming's
additive `cut_with_stats`. Wyoming joins its source pump before counting retained
source and queue frames; prior capacity evictions are excluded. Only the session
writes the early stop, and playback still requires real `played`. `reply.cut`
records `bridge_dropped`, `transport_dropped` and their sum `dropped_frames`.
Neither partial resampler tails nor audio already transmitted are counted.

The pinned-host timing regression reuses one bridge and host for ten trials.
The host's FIFO observer timestamps cancellation dispatch at playback start plus
one second, before instructing the fake to send the event. Sink EOF minus that
timestamp includes controller/pipe dispatch overhead. It never substitutes
`played` for sink closure or treats a missing EOF as zero. The fake offers 150
encoded 20 ms frames per trial. No physical silence or provider claim follows.

Remaining cancellation failure cases and reuse/recovery remain in this lane.
Until those increments are complete the lane is not ready for acceptance.
