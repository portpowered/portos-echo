---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: accepted; fallback echo cancellation in the bridge, off by default
---

# WebRTC echo canceller in the bridge

## Context

The Echo's microphone path gives no echo cancellation
([baselines](../program/baselines.md#the-device)): hassmic's capture source
equals the raw microphone, and the vendor's "voice communication" source
mutes the speaker instead of cancelling. The Live session therefore hears
the Echo's own reply and can interrupt it. The owner decided on 2026-11-19
to use WebRTC's standard canceller in the bridge, and later that day to
try the Echo's call-mode canceller first and keep this one as the fallback
([decisions](../program/decisions.md)). `audio.echo_cancel` is therefore
false by default.

## Decision

`echo-audio` depends on `sonora` 0.3.0, pinned exactly, with
`default-features = false`. sonora is a pure Rust port of WebRTC's audio
processing module, including AEC3, its delay estimator and its high-pass
filter. Its licence is BSD-3-Clause, the licence of WebRTC itself, which
`deny.toml` already allows.

The lockfile gains twelve packages: `sonora`, `sonora-aec3`, `sonora-agc2`,
`sonora-common-audio`, `sonora-fft`, `sonora-ns`, `sonora-simd`, `bytemuck`,
`bytemuck_derive`, `derive_more`, `derive_more-impl` and `unicode-xid`.
`cargo deny check` passes for all of them.

`echo-audio` wraps the processor as `EchoCanceller`: one 16 kHz mono
channel, 10 ms frames, the echo canceller and the high-pass filter only.
Noise suppression and gain control stay off. AEC3 keeps its defaults but
for the delay search, widened to 800 ms, and the configured delay hint.
The [architecture](../architecture.md#echo-cancellation) describes how the
bridge feeds it.

## Why a Rust port and not the C++ library

- The bridge is built for `armv7-linux-androideabi` and for Linux hosts.
  The C++ `webrtc-audio-processing` needs meson, a C++ toolchain and
  abseil for each target, and its Rust bindings need `unsafe`, which the
  workspace forbids.
- sonora builds with cargo alone on both targets, with no build script
  that fetches or compiles foreign code.
- It is a port of the same algorithm: the tests in `echo-audio` measure
  more than 40 dB of echo reduction and the true delay to within 8 ms.

## Consequences

- On the 32-bit Echo the canceller runs its scalar code, with no NEON.
  Its processor share there is not measured yet.
- AEC3's suppressor gives up part of a near-end voice when the echo is
  far louder than the voice. The tests pin what is kept at three echo
  levels. Tuning the suppressor is a later decision.
- A newer sonora changes the filter's numbers: the pin moves only with
  the tests rerun and this record updated.
