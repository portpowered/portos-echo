---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Port OS OAuth client dependencies

## Context

The bridge needs device authorization, durable rotating credentials and ES256 proofs.
The auth lane is authorized to add these dependencies by its task packet.

## Decision

Use reqwest without default features, with rustls and JSON, for bounded HTTP requests;
disable redirects so credentials cannot follow an unvalidated destination. Tokio owns
timers and synchronization. Use p256, rand_core, sha2 and base64 for standard ES256
proofs and encoding. Use url for endpoint validation and tempfile for same-directory
atomic replacement. Reuse serde, serde_json, thiserror and echo-contracts. Use the
existing jsonschema family for offline persisted-record contract tests.

Credentials are private to one bridge user. On Unix replacement files use mode 0600;
on Windows they inherit an owner-only directory ACL set through the installed Windows
PowerShell and .NET filesystem API. The helper captures all diagnostics and creates
no visible window. One client owns a store;
callers share it through Arc. No process-wide lock or concurrent independent writers
are supported. This introduces no remote service tests or shared contract changes.

## Packages covered

The following complete newly locked package-name closure includes platform-specific
TLS, socket, cryptographic and HTTP dependencies, and this workspace library:

- `atomic-waker`
- `base16ct`
- `base64ct`
- `block-buffer`
- `bytes`
- `cc`
- `cfg_aliases`
- `const-oid`
- `crypto-bigint`
- `crypto-common`
- `der`
- `digest`
- `ecdsa`
- `echo-mcp-auth`
- `elliptic-curve`
- `errno`
- `fastrand`
- `ff`
- `find-msvc-tools`
- `form_urlencoded`
- `futures-channel`
- `generic-array`
- `group`
- `hmac`
- `http`
- `http-body`
- `http-body-util`
- `httparse`
- `hyper`
- `hyper-rustls`
- `hyper-util`
- `ipnet`
- `linux-raw-sys`
- `log`
- `lru-slab`
- `mio`
- `p256`
- `pem-rfc7468`
- `pkcs8`
- `primeorder`
- `quinn`
- `quinn-proto`
- `quinn-udp`
- `rand_pcg`
- `reqwest`
- `rfc6979`
- `ring`
- `rustc-hash`
- `rustix`
- `rustls`
- `rustls-pki-types`
- `rustls-webpki`
- `sec1`
- `serde_urlencoded`
- `sha2`
- `shlex`
- `signature`
- `socket2`
- `spki`
- `subtle`
- `sync_wrapper`
- `tempfile`
- `tinyvec`
- `tokio`
- `tokio-macros`
- `tokio-rustls`
- `tower`
- `tower-http`
- `tower-layer`
- `tower-service`
- `tracing`
- `tracing-core`
- `try-lock`
- `typenum`
- `untrusted`
- `url`
- `want`
- `wasi`
- `wasm-bindgen-futures`
- `web-sys`
- `web-time`
- `webpki-roots`
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
