---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: accepted for A2
---

# ADR 0038 A2 audio allocation boundaries

The Wyoming connection owns one reusable read buffer and one reusable write
buffer. The reader starts with 8,704 bytes of scratch, bounded by the
configured frame limit. It projects ordinary inline or segmented audio
headers directly to typed PCM metadata. Control events and exceptional
audio headers use the shared Wyoming decoder and its inline/segment merge.
The wire limit still includes the header, JSON segment and payload. An
unusually large valid frame can grow the read scratch for that connection.

At the default queue capacity, the connection keeps 64 reusable microphone
slots and two more for the reader and its one-entry handoff to the session.
Each holds up to 8,192 payload bytes, for 540,672 bytes of PCM capacity.
Custom queue depths below 64 keep the same two handoff slots. Larger
configured depths keep their queue limit and can use owned overflow storage.
The Wyoming and EchoLink microphone frames move the same lease. Dropping or
evicting a frame releases its slot. A larger wire chunk or a caller retaining
more slots than the pool can supply uses an owned payload. Neither case blocks
the reader or changes the bridge's PCM admission limit.

The writer starts with 1,472 bytes of scratch and reuses it for the canonical
audio header and complete PCM frame. It checks the complete size before
writing. Extended headers and control frames use the canonical encoder.
The media encoder retains a 1,275-byte Opus scratch buffer. WebRTC receives
one owned copy of the encoded prefix at the track write. Resampling and
receive generation still follow the existing codec and call ownership.

The pacer retains its warmed mouth transition and lip analysis storage on
reset. Session turn marks retain their staging capacity after the voice
service drains them. The lip adapter converts up to five samples on the
stack. The existing ordered SSE queues carry typed samples inline, and the
stream formats the named event when it is read. The 24 reserved slots still
carry screen frames. The service accepts larger compatibility batches with
shared owned sample storage.

`make test-alloc` runs the isolated T3 allocation test once on Rust unit shard
1 of 4, or once with an unpartitioned unit target. It counts synchronous
World steps and scheduled ticks on one paused Tokio thread. Each of the seven
paths exercises 500 frames after warming with 64. The service windows include
work on the observed thread, including the test peer, runtime and verification
work. Those counts are reported separately from core allocations.

## Deferred

- The allocation metric covers warmed audio and interruption steps. It excludes
  run and connection construction, native Opus allocation, allocator activity
  on other threads, transcript text growth and exceptional capture backing.
- The device clock, replay, ARM profiling and hardware validation remain with
  the existing plan gates.
