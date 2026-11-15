---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Independent ChatGPT credential source

## Status

Accepted for el-w2-chatgpt-auth, plan section 4.2.

## Context

ChatGPT device sign-in belongs in echo-chatgpt-auth, independently of Live and
Port OS OAuth. Configuration remains synchronous and offline. The wire behavior
is reimplemented from the pinned references in the program plan; no upstream
source is copied.

## Decision

Use reqwest with default features disabled, rustls and native certificate roots,
serde JSON and hand-written forms. Disable redirects and environment proxies
so credentials are never forwarded to a redirect target. Use the existing
Secret wrapper, base64 JWT decoding and tokio deadlines. No OAuth framework,
API-key exchange, implicit home credential discovery or real-service test.

Access JWT claims are decoded without signature verification: the issuer is
trusted over TLS. All tests replace every destination with literal loopback.

Create a private temporary directory beside the destination before writing
credential bytes, sync the file and rename it over the destination. Unix uses
0700 directories and 0600 files. Windows protects the temporary directory DACL
with only the current user SID granted FullControl before creating the file;
the renamed file retains this DACL. This does not prevent administrator takeover
or promise power-loss durability on every filesystem. Rejected HTTP bodies,
paths and operating-system diagnostics never appear in AuthError.

## Packages covered

- atomic-waker
- cfg_aliases
- core-foundation
- core-foundation-sys
- echo-chatgpt-auth
- form_urlencoded
- futures-channel
- http
- http-body
- http-body-util
- httparse
- hyper
- hyper-rustls
- hyper-util
- ipnet
- lru-slab
- openssl-probe
- quinn
- quinn-proto
- quinn-udp
- rand_pcg
- reqwest
- ring
- rustls
- rustls-native-certs
- rustls-webpki
- schannel
- security-framework
- security-framework-sys
- serde_urlencoded
- sync_wrapper
- tinyvec
- tokio-rustls
- tower
- tower-http
- tower-layer
- tower-service
- try-lock
- url
- want
- wasm-bindgen-futures
- web-sys
- web-time
- windows-targets
- windows_aarch64_gnullvm
- windows_aarch64_msvc
- windows_i686_gnu
- windows_i686_gnullvm
- windows_i686_msvc
- windows_x86_64_gnu
- windows_x86_64_gnullvm
- windows_x86_64_msvc
