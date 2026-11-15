---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# PCM resampling and native Opus

## Context

The bridge needs bounded streaming conversion between 16, 22.05, 24 and 48 kHz
signed 16-bit PCM and 48 kHz mono Opus. This crate owns no transport or hardware.

## Decision

Use rubato 0.16.2's sinc resampler with FFT features disabled. Keep phase across
caller chunks, compensate its declared output delay and trim the finite stream
to its duration-derived length. Process at most one 20 ms input block per call.
Equal-rate PCM bypasses filtering to preserve exact samples. Stereo downmix
uses an i32 sum divided by two, rounding toward zero before filtering.

Use opus 0.4.0's safe API with bundled static libopus through opusic-sys. CMake
and a native C/C++ compiler are build prerequisites on Windows and Linux. No
owned unsafe code, system-library fallback, hand-rolled codec or new network
boundary is introduced. The codec accepts only complete 960-sample mono frames
and validates packet shape and duration before decoding, without implicit PLC.
Native processing failures require processor reset or recreation. Codec quality
is measured at fixed 64 kbps with codec-reported lookahead only.

These are additive dependencies under the audio packet's explicit dependency
authorization. Reverting the crate and regenerating Cargo.lock rolls them back
before downstream consumers land. Dependency FFI remains outside this lib's
unsafe-forbidden boundary. Native bundled compilation must be proven on the
Windows host; hosted Linux build and independent replay remain review gates.

## Queue and pacing

Use Tokio time for a single-owner 20 ms pacer with its first deadline one
period after construction. Await before removing a complete queue entry;
cancellation preserves the entry and deadline. A late call emits once and
schedules the next deadline after now, avoiding catch-up bursts. The caller
sets the send lead by choosing the fixed positive FrameQueue entry capacity
(10 entries represents 200 ms for 20 ms frames). Clear the queue and recreate
the pacer between streams. No task, channel or transport is introduced.

Use the existing proptest dependency for bounded FIFO operation sequences and
loom for a small model around the actual queue under external synchronization.
Tokio runtime/macros/test-util features are test-only, for deterministic paused
clock proof. The dependency declarations are explicitly authorized by the audio
packet. All added transitive packages below belong to this dependency closure.

## Packages covered

- echo-audio
- opus
- opusic-sys
- rubato
- cc
- cmake
- find-msvc-tools
- shlex
- generator
- log
- loom
- matchers
- nu-ansi-term
- scoped-tls
- sharded-slab
- thread_local
- tokio
- tokio-macros
- tracing
- tracing-core
- tracing-log
- tracing-subscriber
- valuable
- windows-core
- windows-implement
- windows-interface
- windows-link
- windows-result
- windows-strings
- proptest
