---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: proposed; dependency admission required before implementation
---

# Frame-based Codex media peer

## Context

The app server owns authenticated call creation and the sideband, while the
bridge must supply its own WebRTC audio peer. The existing echo-live path does
not supply the decoded PCM boundary required by the app-server cutover.
This library is additive; existing callers remain in place until that cutover.

## Source and attribution

Use OpenAI Codex release rust-v0.160.0, commit
`a956835d020762cb2b570053af06f643a11c0ecc`, licensed Apache-2.0. The repository
NOTICE preserves the complete upstream notice, including its Ratatui paragraph.
Adapted files must retain attribution, carry SPDX Apache-2.0 identification and
state "Modified by Port OS".

The source paths under `codex-rs/voice-host/src/` are `transport.rs`,
`audio_track.rs`, `processing.rs`, `incoming.rs` and `playout.rs`.
`codex-rs/realtime-webrtc/src/{protocol,session,client}.rs` supplies the reference
for bounded, redacted signaling and lifecycle ownership, rather than a helper
process dependency. The [research brief](../spikes/codex-app-server.md) describes
main `5b3d2c7fce`; that revision is not the adaptation pin.

## Design

Retain the pinned single Opus track, 48 kHz RTP clock, 20 ms packets,
`opus/48000/2` capability and ordered `oai-events` channel. The internal PCM and
encoder remain mono. Use persistent Rubato conversion between 16 kHz caller
frames and 48 kHz codec frames in both directions. Replace upstream GStreamer
playout with the safe Opus decoder and receive-side Rubato conversion. Omit
cpal, Sonora, plugin loading, device discovery and helper-process framing.

The pinned transport binds UDP and TCP wildcard addresses for both IP families;
webrtc expands those bindings into usable local host candidates. Production
defaults must admit every usable non-loopback interface, with an optional
assigned-address allowlist and no implicit STUN service. Local tests explicitly
select loopback. No LAN or provider reachability follows from synthetic SDP
tests or local-pair connectivity.

Replace upstream ingress's discard-new pressure policy with bounded oldest-drop
queues and exact counters. Control and state updates must remain separate from
lossy audio. Connected requires actual transport and event-channel readiness.
Closed is terminal, discards buffered media and joins owned tasks. SDP admission
must enforce the upstream 32-candidate occurrence bound before peer mutation.

## Dependency admission and native linkage

The intended direct versions are webrtc/rtc 0.20.3, opus 0.4.0, rubato 5.0.0,
rand 0.9 and Tokio 1. No dependency is introduced by this documentation change.
Before admitting the library, its entire resolved dependency closure must meet
the existing cargo-deny advisory and licence policy, and each new package and
selected licence must be recorded here. A pin does not authorize an advisory
exception or replacement of a transitive dependency outside its version range.
Changing the selected pin or introducing a dependency patch needs an explicit
coordinator decision before implementation proceeds.

Follow [the native audio precedent](0008-echo-audio.md): opus uses opusic-sys's
bundled static libopus, built with CMake and native MSVC on Windows or GCC/Clang
on Linux. Owned Rust code remains unsafe-forbidden. Actual linkage and tone
behavior require native builds on both platforms; this design is not proof of
either build. No workspace, CI, coverage or advisory policy change is proposed.
