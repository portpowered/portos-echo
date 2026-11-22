---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 22
status: accepted
---

# Direct Muse gadget protocol boundary

## Context

The direct-gadget design calls for the bridge to own the Muse gadget connection.
M1 delivers the protocol library independently of deployment. The existing
task rules are recorded in [program plan section 7](../program/plan.md#7-muse-a-wrapper-around-the-gadget-sdk).

## Decision

`echo-svc-muse` is a Linux library with a narrow async service boundary using
the shared task types. The caller supplies the prepared pairing file and the
same PEM roots packaged with Codex. The library checks both blocked root
identities by certificate and public-key fingerprint. TLS authenticates the
VM hostname and chain. Noise XX establishes the encrypted daemon stream.

The VM sees a single advertised command, `echo.report.result`, on the
acknowledged control stream. The framing codec uses the reference protobuf
field numbers, bounded reassembly, and a 256 KiB control-message limit.

`x25519-dalek` 2.0.1 implements X25519 with a persistent static key. Its new
lockfile dependencies are `curve25519-dalek`, `curve25519-dalek-derive`,
`fiat-crypto`, and `zeroize_derive`. The existing `ring` package provides
AES-256-GCM, HKDF-SHA256, SHA-256 and ephemeral randomness. The existing
rustls, reqwest and tokio-tungstenite packages provide the network boundary.

## Evidence

The crate suite checks the published Noise vector, byte-level framing and
pairing admission. Its component tests use a Rust fake API and VM with
runtime-generated TLS credentials on loopback. The source design and SDK pin
are recorded in the [direct-gadget design](https://github.com/portpowered/portos-echo/blob/f3f5bd49/docs/design/2026-11-22-muse-direct-gadget.md).
