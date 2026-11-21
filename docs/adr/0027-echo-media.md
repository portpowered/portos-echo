---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: accepted; additive media library
---

# Frame-based Codex media peer

## Context

The app server owns authenticated call creation and the sideband, while the
bridge must supply its own WebRTC audio peer. The existing echo-live path does
not supply the decoded PCM boundary required by the app-server cutover.
This library is additive; existing callers remain in place until that cutover.

## Source and attribution

The media peer was adapted from OpenAI Codex release rust-v0.160.0, commit
`a956835d020762cb2b570053af06f643a11c0ecc`, licensed Apache-2.0. The repository
NOTICE preserves the complete upstream notice, including its Ratatui paragraph.
Adapted files must retain attribution, carry SPDX Apache-2.0 identification and
state "Modified by Port OS".
The runtime app-server release is selected separately by [codex.pin](../../codex.pin).

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

The admitted direct versions are webrtc/rtc 0.21.1, opus 0.4.0, rubato 5.0.0,
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

## Coordinator dependency amendment

Mailbox response 1 (2026-11-16 20:05 PST), Option B, replaces voice-host's
webrtc/rtc =0.20.3 with =0.21.1. The design and Codex source pin stay unchanged.
The old closure required vulnerable crypto-bigint 0.5 through p256/p384;
the 0.21 crypto-provider design removes that path. No advisory exceptions,
patched sources or further direct dependencies are authorized. The decision
is recorded in [the program log](../program/decisions.md).

The API adaptations preserve the same peer/track/ingress design:

- SettingEngine uses SettingEngineBuilder and `with_*` methods; the SCTP
  receive bound moves from the peer builder to that settings builder.
- Interceptors implement Protocol and Interceptor explicitly; Registry holds
  a flat slotted chain instead of deriving the former nested interceptor.
  RTP is now inside `TaggedPacket.message.packet`. Our final application slot
  consumes it before the upstream track queue, as the pin did.
- TrackLocalStaticSample and Sample require an explicit Instant. Production
  sample/ICE timing uses the Tokio runtime clock; the runtime adapter exposes
  virtual time for deterministic deadline checks and retains the pin's
  32-stream inbound TCP admission bound.
- Enumerate via rtc's existing shared interface facade, pass admitted assigned
  IP addresses to both UDP and TCP bind paths, and fail with InvalidConfig
  when none remain. This avoids wildcard fallback advertising unspecified IPs.
  The socket-free SDP test passes controlled documentation addresses through
  the identical admission policy and the real rtc Sans-I/O SDP emitter.
- Preserve the ordered oai-events offer; the private test answer role adopts
  that channel. Production has no answering or helper-process API.

## Streaming and resource policy

Each direction has persistent Rubato Sinc history (320 input samples at 16 kHz
or 960 at 48 kHz), mono Opus, and fixed-size 20 ms PCM frames. The RTP sequence
check discards duplicate/late packets before touching codec state; invalid
payloads fail the session using fixed Error categories. A full queue evicts its
oldest item and increments the relevant cumulative counter. Raw RTP ingress
and decoded PCM share the receive counter; control/state never enters an audio
queue. A negotiated deadline stops unanswered sessions. Close discards audio,
cancels and joins owned workers and closes both transports; Drop requests the
same cancellation without blocking.

The tone oracle uses one fixed 250–500 ms steady-state window, past Sinc/Opus
startup delay, and scans 50–7950 Hz with Goertzel followed by a one-hertz
refinement. It checks frequency before duration/amplitude, so skipping outbound
conversion exposes the requested threefold frequency error. Its signal grid is
20 ms; connection elapsed time uses the expressly required monotonic clock.
No synthetic SDP or local fake proves real LAN, NAT or provider connectivity.

## Added lockfile closure and selected licenses

Every newly resolved package/version is listed below (including the workspace
crate). MIT is selected when an MIT alternative exists; otherwise Apache-2.0
or the explicitly named compatible license applies. Existing opusic-sys bundles
libopus under BSD-3-Clause; opus itself is Apache-2.0. No license allow-list
change is needed. Cargo regenerates the lockfile; existing echo-audio continues
using Rubato 0.16.2.

| Package | Version | Declared license | Selected license |
| --- | --- | --- | --- |
| aead | 0.5.2 | MIT OR Apache-2.0 | MIT |
| aes | 0.8.4 | MIT OR Apache-2.0 | MIT |
| asn1-rs | 0.6.2 | MIT OR Apache-2.0 | MIT |
| asn1-rs-derive | 0.5.1 | MIT OR Apache-2.0 | MIT |
| async-broadcast | 0.7.2 | MIT OR Apache-2.0 | MIT |
| async-channel | 2.5.0 | Apache-2.0 OR MIT | MIT |
| async-trait | 0.1.92 | MIT OR Apache-2.0 | MIT |
| audio-codec-algorithms | 0.8.1 | 0BSD OR Apache-2.0 | Apache-2.0 |
| audioadapter | 5.0.0 | MIT OR Apache-2.0 | MIT |
| audioadapter-buffers | 5.2.0 | MIT OR Apache-2.0 | MIT |
| audioadapter-sample | 5.2.0 | MIT OR Apache-2.0 | MIT |
| bitflags | 1.3.2 | MIT/Apache-2.0 | MIT |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | MIT |
| bytecheck | 0.8.3 | MIT | MIT |
| bytecheck_derive | 0.8.3 | MIT | MIT |
| byteorder | 1.5.0 | Unlicense OR MIT | MIT |
| ccm | 0.5.0 | Apache-2.0 OR MIT | MIT |
| cipher | 0.4.4 | MIT OR Apache-2.0 | MIT |
| concurrent-queue | 2.5.0 | Apache-2.0 OR MIT | MIT |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | MIT |
| crc32c | 0.6.9 | MIT OR Apache-2.0 | MIT |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 | MIT |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | MIT |
| ctr | 0.9.2 | MIT OR Apache-2.0 | MIT |
| der-parser | 9.0.0 | MIT/Apache-2.0 | MIT |
| digest | 0.10.7 | MIT OR Apache-2.0 | MIT |
| echo-media | 0.1.0 | Apache-2.0 | Apache-2.0 |
| event-listener | 5.4.2 | Apache-2.0 OR MIT | MIT |
| event-listener-strategy | 0.5.4 | Apache-2.0 OR MIT | MIT |
| futures | 0.3.34 | MIT OR Apache-2.0 | MIT |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 | MIT |
| generic-array | 0.14.7 | MIT | MIT |
| hex | 0.4.3 | MIT OR Apache-2.0 | MIT |
| hmac | 0.12.1 | MIT OR Apache-2.0 | MIT |
| inout | 0.1.4 | MIT OR Apache-2.0 | MIT |
| md-5 | 0.10.6 | MIT OR Apache-2.0 | MIT |
| memoffset | 0.9.1 | MIT | MIT |
| munge | 0.4.7 | MIT | MIT |
| munge_macro | 0.4.7 | MIT | MIT |
| nix | 0.31.4 | MIT | MIT |
| oid-registry | 0.7.1 | MIT OR Apache-2.0 | MIT |
| parking | 2.2.1 | Apache-2.0 OR MIT | MIT |
| pem | 3.0.6 | MIT | MIT |
| pem | 4.0.0 | MIT | MIT |
| ptr_meta | 0.3.2 | MIT | MIT |
| ptr_meta_derive | 0.3.2 | MIT | MIT |
| quinn-udp | 0.6.3 | MIT OR Apache-2.0 | MIT |
| rancor | 0.1.3 | MIT | MIT |
| rend | 0.5.4 | MIT | MIT |
| rkyv | 0.8.18 | MIT | MIT |
| rkyv_derive | 0.8.18 | MIT | MIT |
| rtc | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-crypto | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-datachannel | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-dtls | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-ice | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-interceptor | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-mdns | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-media | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-rtcp | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-rtp | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-sctp | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-sdp | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-shared | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-srtp | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-stun | 0.21.1 | MIT/Apache-2.0 | MIT |
| rtc-turn | 0.21.1 | MIT/Apache-2.0 | MIT |
| rubato | 5.0.0 | MIT OR Apache-2.0 | MIT |
| sansio | 1.0.1 | MIT/Apache-2.0 | MIT |
| sha1 | 0.10.7 | MIT OR Apache-2.0 | MIT |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 | MIT |
| substring | 1.4.5 | MIT OR Apache-2.0 | MIT |
| thiserror | 1.0.69 | MIT OR Apache-2.0 | MIT |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 | MIT |
| unicase | 2.10.0 | MIT OR Apache-2.0 | MIT |
| visibility | 0.1.1 | Zlib OR MIT OR Apache-2.0 | MIT |
| webrtc | 0.21.1 | MIT/Apache-2.0 | MIT |
| winapi | 0.3.9 | MIT/Apache-2.0 | MIT |
| winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | MIT |
| winapi-x86_64-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | MIT |
| windowfunctions | 0.1.1 | MIT | MIT |
| x509-parser | 0.16.0 | MIT OR Apache-2.0 | MIT |
