---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted for the provisional local adapter; actual provider validation remains M2
---

# 0006: Provisional Live call adapter and synthetic server

Use reqwest 0.12 with default features disabled, JSON and native-root Rustls TLS,
plus existing serde, serde_json, Tokio and echo-contracts. No OAuth dependency
belongs here: the bridge supplies a Secret-backed credential provider. No
credential-bearing type implements Debug or Display. HTTP redirects, retries
and environment proxies are disabled; errors discard URLs and remote bodies.
HTTPS origins are accepted, with plaintext HTTP restricted to literal 127.0.0.1
for the synthetic server. The response is bounded to 64 KiB.

The adapter posts exactly one JSON {sdp, session} request, with client delegation
only, and consumes a raw SDP answer. These are provisional adapter conventions,
not verified OpenAI wire formats. ChatGPT mode uses the section 4 route, query
and correlation headers; API-key mode uses /v1/live/sessions without those
headers. Model, voice and instructions are explicit library inputs. M2 owns
real-provider compatibility, entitlement, real traces and sideband decisions.

Creation owns a peer until HTTP signaling and ICE/data-channel readiness finish.
One deadline bounds credential retrieval, HTTP and readiness. Cancelling creation
drops the peer; no retry worker survives. A call decodes the unchanged provisional
LiveEvent and serializes matching commentary. Commentary has a 16 KiB limit after
JSON escaping. The caller drives receive, services protocol timers and drains audio;
close releases local resources idempotently, with no invented remote DELETE route.
Cancel an uncertain send or close a failed decode/pressure journey rather than
replaying accepted messages. Already received audio remains available after close.

The default feature set excludes the synthetic server. The fake feature exports
FakeLive, FakeScript and private-comparison request observations for later sim
consumers. Its sockets bind ephemeral 127.0.0.1 addresses. Each request has its own
LivePeer, script, frame count and observer record. A root task owns a JoinSet of
per-call tasks; dropping/shutting down the server aborts them. The bounded local
HTTP parser supports only the controlled POST with Content-Length. It is not a
production HTTP listener. No credential or raw account value is printed by the
observer; matching methods return booleans.

Run the packet's call_logic_, call_wire_ and t3_call_ tests, the fake-feature build
and clippy, then make ci. The T3 paths use real HTTP and ICE/DTLS/SRTP/SCTP on
loopback with synthetic scripts and encoded packets, not provider or device proof.
Independent acceptance, terminal CI, clean-room loopback and merge remain review
owned (R-RUNTIME, R-CI, R-LOOP, R-MERGE). Assembled W1 and actual provider validation
remain L-W1 and L-M2. Rollback removes the adapter/fake and regenerates Cargo.lock;
there is no persisted state or shared-contract migration.

## Packages covered

Selected direct dependencies (including already locked names):

- `echo-contracts`
- `reqwest`
- `serde`
- `serde_json`
- `tokio`

New resolved package names, including inactive and target-specific entries:

- `atomic-waker`
- `cfg_aliases`
- `core-foundation`
- `core-foundation-sys`
- `form_urlencoded`
- `futures-channel`
- `http`
- `http-body`
- `http-body-util`
- `httparse`
- `hyper`
- `hyper-rustls`
- `hyper-util`
- `ipnet`
- `lru-slab`
- `openssl-probe`
- `quinn`
- `quinn-proto`
- `quinn-udp`
- `rand_pcg`
- `reqwest`
- `ring`
- `rustls`
- `rustls-native-certs`
- `rustls-webpki`
- `schannel`
- `security-framework`
- `security-framework-sys`
- `serde_urlencoded`
- `sync_wrapper`
- `tinyvec`
- `tokio-rustls`
- `tower`
- `tower-http`
- `tower-layer`
- `tower-service`
- `try-lock`
- `url`
- `want`
- `wasm-bindgen-futures`
- `web-sys`
- `web-time`
- `windows-targets`
- `windows_aarch64_gnullvm`
- `windows_aarch64_msvc`
- `windows_i686_gnu`
- `windows_i686_gnullvm`
- `windows_i686_msvc`
- `windows_x86_64_gnu`
- `windows_x86_64_gnullvm`
- `windows_x86_64_msvc`
