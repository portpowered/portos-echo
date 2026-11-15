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

Use reqwest 0.12 without default features, with rustls native roots and JSON, for bounded HTTP requests;
disable redirects so credentials cannot follow an unvalidated destination. Tokio owns
timers and synchronization. Use p256 0.14, rand_core and base64 for standard ES256
proofs and encoding. Use url for endpoint validation and tempfile for same-directory
atomic replacement. Reuse serde, serde_json, thiserror and echo-contracts. Use the
existing jsonschema family for offline persisted-record contract tests.

Credentials are private to one bridge user. On Unix replacement files use mode 0600;
on Windows they inherit an owner-only directory ACL set through the installed Windows
PowerShell and .NET filesystem API. The helper captures all diagnostics and creates
no visible window. One client owns a store;
callers share it through Arc. No process-wide lock or concurrent independent writers
are supported. This introduces no remote service tests or shared contract changes.

The coordinator authorized p256 0.14 and native trust roots on 2026-11-15.
The crypto-bigint selection must be at least 0.7.6; advisory and license gates
remain unchanged. Rustls verifies certificates against the OS trust store on
Windows and Ubuntu; native-tls, OpenSSL and bundled webpki roots are disabled.
The library uses p256's Generate trait and SEC1 API. SHA-256 is supplied by
p256's closure; no unused direct sha2 dependency is retained.

## Packages covered

The complete new lockfile closure, including platform-specific packages, is:

| Package | Version | License |
| --- | --- | --- |
| `atomic-waker` | 1.1.2 | Apache-2.0 / MIT |
| `base16ct` | 1.0.0 | Apache-2.0 / MIT |
| `base64` | 0.23.1 | MIT / Apache-2.0 |
| `base64ct` | 1.8.3 | Apache-2.0 / MIT |
| `block-buffer` | 0.12.1 | MIT / Apache-2.0 |
| `bytes` | 1.12.1 | MIT |
| `cc` | 1.11.2 | MIT / Apache-2.0 |
| `cfg_aliases` | 0.2.2 | MIT |
| `cmov` | 0.5.4 | Apache-2.0 / MIT |
| `const-oid` | 0.10.3 | Apache-2.0 / MIT |
| `core-foundation` | 0.10.1 | MIT / Apache-2.0 |
| `core-foundation-sys` | 0.8.7 | MIT / Apache-2.0 |
| `cpubits` | 0.1.1 | MIT / Apache-2.0 |
| `crypto-bigint` | 0.7.11 | Apache-2.0 / MIT |
| `crypto-common` | 0.2.2 | MIT / Apache-2.0 |
| `ctutils` | 0.4.8 | Apache-2.0 / MIT |
| `der` | 0.8.3 | Apache-2.0 / MIT |
| `digest` | 0.11.3 | MIT / Apache-2.0 |
| `ecdsa` | 0.17.0 | Apache-2.0 / MIT |
| `echo-mcp-auth` | 0.1.0 | Apache-2.0 |
| `elliptic-curve` | 0.14.3 | Apache-2.0 / MIT |
| `errno` | 0.3.15 | MIT / Apache-2.0 |
| `fastrand` | 2.5.0 | Apache-2.0 / MIT |
| `ff` | 0.14.0 | MIT/Apache-2.0 |
| `find-msvc-tools` | 0.1.15 | MIT / Apache-2.0 |
| `form_urlencoded` | 1.2.2 | MIT / Apache-2.0 |
| `futures-channel` | 0.3.34 | MIT / Apache-2.0 |
| `getrandom` | 0.2.17 | MIT / Apache-2.0 |
| `group` | 0.14.0 | MIT/Apache-2.0 |
| `hmac` | 0.13.0 | MIT / Apache-2.0 |
| `http` | 1.5.0 | MIT / Apache-2.0 |
| `http-body` | 1.1.0 | MIT |
| `http-body-util` | 0.1.5 | MIT |
| `httparse` | 1.10.1 | MIT / Apache-2.0 |
| `hybrid-array` | 0.4.15 | MIT / Apache-2.0 |
| `hyper` | 1.12.0 | MIT |
| `hyper-rustls` | 0.27.10 | Apache-2.0 / ISC / MIT |
| `hyper-util` | 0.1.21 | MIT |
| `ipnet` | 2.12.2 | MIT / Apache-2.0 |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception / Apache-2.0 / MIT |
| `log` | 0.4.35 | MIT / Apache-2.0 |
| `lru-slab` | 0.1.3 | MIT / Apache-2.0 / Zlib |
| `mio` | 1.2.5 | MIT |
| `openssl-probe` | 0.2.2 | MIT / Apache-2.0 |
| `p256` | 0.14.0 | Apache-2.0 / MIT |
| `pem-rfc7468` | 1.0.0 | Apache-2.0 / MIT |
| `pkcs8` | 0.11.0 | Apache-2.0 / MIT |
| `primefield` | 0.14.0 | Apache-2.0 / MIT |
| `primeorder` | 0.14.0 | Apache-2.0 / MIT |
| `quinn` | 0.11.12 | MIT / Apache-2.0 |
| `quinn-proto` | 0.11.19 | MIT / Apache-2.0 |
| `quinn-udp` | 0.5.16 | MIT / Apache-2.0 |
| `rand_core` | 0.6.4 | MIT / Apache-2.0 |
| `rand_pcg` | 0.10.2 | MIT / Apache-2.0 |
| `reqwest` | 0.12.28 | MIT / Apache-2.0 |
| `rfc6979` | 0.6.0 | Apache-2.0 / MIT |
| `ring` | 0.17.14 | Apache-2.0 AND ISC |
| `rustc-hash` | 2.1.3 | Apache-2.0 / MIT |
| `rustix` | 1.1.6 | Apache-2.0 WITH LLVM-exception / Apache-2.0 / MIT |
| `rustls` | 0.23.46 | Apache-2.0 / ISC / MIT |
| `rustls-native-certs` | 0.8.5 | Apache-2.0 / ISC / MIT |
| `rustls-pki-types` | 1.16.0 | MIT / Apache-2.0 |
| `rustls-webpki` | 0.103.17 | ISC |
| `schannel` | 0.1.30 | MIT |
| `sec1` | 0.8.1 | Apache-2.0 / MIT |
| `security-framework` | 3.7.0 | MIT / Apache-2.0 |
| `security-framework-sys` | 2.17.0 | MIT / Apache-2.0 |
| `serde_urlencoded` | 0.7.1 | MIT/Apache-2.0 |
| `serdect` | 0.4.3 | Apache-2.0 / MIT |
| `sha2` | 0.11.1 | MIT / Apache-2.0 |
| `shlex` | 2.0.1 | MIT / Apache-2.0 |
| `signature` | 3.0.0 | Apache-2.0 / MIT |
| `socket2` | 0.6.5 | MIT / Apache-2.0 |
| `spki` | 0.8.1 | Apache-2.0 / MIT |
| `subtle` | 2.6.1 | BSD-3-Clause |
| `sync_wrapper` | 1.0.2 | Apache-2.0 |
| `tempfile` | 3.27.0 | MIT / Apache-2.0 |
| `tinyvec` | 1.13.3 | Zlib / Apache-2.0 / MIT |
| `tokio` | 1.54.1 | MIT |
| `tokio-macros` | 2.7.2 | MIT |
| `tokio-rustls` | 0.26.6 | MIT / Apache-2.0 |
| `tower` | 0.5.3 | MIT |
| `tower-http` | 0.6.11 | MIT |
| `tower-layer` | 0.3.3 | MIT |
| `tower-service` | 0.3.3 | MIT |
| `tracing` | 0.1.44 | MIT |
| `tracing-core` | 0.1.36 | MIT |
| `try-lock` | 0.2.5 | MIT |
| `typenum` | 1.20.1 | MIT / Apache-2.0 |
| `untrusted` | 0.9.0 | ISC |
| `url` | 2.5.8 | MIT / Apache-2.0 |
| `want` | 0.3.2 | MIT |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception / Apache-2.0 / MIT |
| `wasm-bindgen-futures` | 0.4.80 | MIT / Apache-2.0 |
| `web-sys` | 0.3.107 | MIT / Apache-2.0 |
| `web-time` | 1.1.0 | MIT / Apache-2.0 |
| `windows-sys` | 0.52.0 | MIT / Apache-2.0 |
| `windows-targets` | 0.52.6 | MIT / Apache-2.0 |
| `windows_aarch64_gnullvm` | 0.52.6 | MIT / Apache-2.0 |
| `windows_aarch64_msvc` | 0.52.6 | MIT / Apache-2.0 |
| `windows_i686_gnu` | 0.52.6 | MIT / Apache-2.0 |
| `windows_i686_gnullvm` | 0.52.6 | MIT / Apache-2.0 |
| `windows_i686_msvc` | 0.52.6 | MIT / Apache-2.0 |
| `windows_x86_64_gnu` | 0.52.6 | MIT / Apache-2.0 |
| `windows_x86_64_gnullvm` | 0.52.6 | MIT / Apache-2.0 |
| `windows_x86_64_msvc` | 0.52.6 | MIT / Apache-2.0 |
| `wnaf` | 0.14.1 | Apache-2.0 / MIT |
| `zeroize` | 1.9.1 | Apache-2.0 / MIT |
