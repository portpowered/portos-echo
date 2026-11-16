---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Stateless MCP HTTP transport and local counterpart

## Context

The bridge needs POST-only MCP with a current Bearer value on every request,
without an OAuth dependency. Tests need a deterministic loopback server and a
standalone executable using the same implementation.

## Decision

Use reqwest without default features, with rustls TLS using native certificate roots, JSON and streamed bodies.
Disable redirects, proxy inheritance and transport retries. Use axum for the
loopback server, Tokio for bounded asynchronous work and serde for wire values.
Keep remote diagnostics out of errors and records. Bound bodies, catalogs,
concurrency and retained arrivals; refuse capacity rather than evict evidence.
Native roots preserve HTTPS certificate validation without webpki-roots, whose
CDLA-Permissive-2.0 license is outside the standing allowlist. The proposed
rustls feature is conservatively specialized without a license exception.
These dependencies are authorized by the el-w1-mcp packet. No shared contract
changes, real services or device access are involved.

## Packages covered

The cargo-generated lock closure is listed below (including new versions of existing names).

- `atomic-waker`
- `axum`
- `axum-core`
- `base64`
- `bytes`
- `cfg_aliases`
- `core-foundation`
- `core-foundation-sys`
- `echo-mcp-client`
- `echo-spoof-mcp`
- `errno`
- `form_urlencoded`
- `futures-channel`
- `futures-io`
- `futures-macro`
- `futures-sink`
- `getrandom`
- `http`
- `http-body`
- `http-body-util`
- `httparse`
- `httpdate`
- `hyper`
- `hyper-rustls`
- `hyper-util`
- `ipnet`
- `lru-slab`
- `matchit`
- `mime`
- `mio`
- `openssl-probe`
- `quinn`
- `quinn-proto`
- `quinn-udp`
- `rand_pcg`
- `reqwest`
- `ring`
- `rustc-hash`
- `rustls`
- `rustls-native-certs`
- `rustls-pki-types`
- `rustls-webpki`
- `schannel`
- `security-framework`
- `security-framework-sys`
- `serde_path_to_error`
- `serde_urlencoded`
- `signal-hook-registry`
- `socket2`
- `subtle`
- `sync_wrapper`
- `tinyvec`
- `tokio-rustls`
- `tokio-util`
- `tower`
- `tower-http`
- `tower-layer`
- `tower-service`
- `tracing-attributes`
- `try-lock`
- `untrusted`
- `url`
- `want`
- `wasi`
- `wasm-bindgen-futures`
- `wasm-streams`
- `web-sys`
- `web-time`
- `windows-sys`
- `windows-targets`
- `windows_aarch64_gnullvm`
- `windows_aarch64_msvc`
- `windows_i686_gnu`
- `windows_i686_gnullvm`
- `windows_i686_msvc`
- `windows_x86_64_gnu`
- `windows_x86_64_gnullvm`
- `windows_x86_64_msvc`
- `zeroize`
