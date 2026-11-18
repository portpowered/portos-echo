---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Independent ChatGPT credential source

## Canonical cutover status (2026-11-17)

Codex app-server supersedes the canonical bridge implementation of this path.
The APIs described here remain active for retained sign-in/provider adapters,
legacy/Muse bridge execution and local fake consumers. The retirement lane
retains consumed behavior and all existing quality gates; it does not authorize
removing these APIs or migrating their callers.

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

Serialize same-source operations with tokio and independent sources with an OS
file lock beside the credential. The empty lock file persists to preserve inode
identity; the OS releases its lock on process exit. A contender returns Busy.
Before posting a refresh, sync a non-secret pending byte in the lock file.
Clear it after durable commit. A lost response, failed commit or cancellation
leaves it pending, so the next due call requires sign-in instead of replaying a
possibly consumed token. A fresh committed record or successful sign-in clears it.
Use the existing time package for RFC3339 import timestamps and loom for the
bounded rotation/publication model. Import never discovers a home path.

Browser sign-in uses sha2 for the S256 challenge and getrandom for independent
32-byte OS-random verifier and state values. A loopback listener is held before
reporting the URL; an explicit redirect port of zero selects an OS-assigned port
and uses that same resolved redirect in authorization and exchange. The listener,
bounded callback reads and exchange are owned by the future, with no background
tasks. This library does not launch or automate a browser or consent UI.

## Packages covered

- atomic-waker
- cfg_aliases
- core-foundation
- core-foundation-sys
- echo-chatgpt-auth
- form_urlencoded
- getrandom
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
- loom
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
- sha2
- sync_wrapper
- tinyvec
- time
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
