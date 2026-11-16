---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Loopback screen transport

`echo-screen-server` is a standalone library using the existing `echo-screen-contract`
types and merged screen page. It reuses locked axum, futures-util, serde, serde_json,
sha2, thiserror, tokio, hyper and hyper-util packages; no new third-party package
is introduced. Owned hyper connections allow cancellation to drop a socket even
when its writer stalls; the axum router supplies HTTP/SSE semantics.
The bridge wiring lane owns product pairing and trace mapping.

The constructor rejects non-loopback addresses. Only a SHA-256 digest wrapped in
`Secret` survives construction. Protected requests use the page's existing query
token grammar; duplicate tokens are refused. Static assets carry no state or token.
All responses disable caching and referrer propagation; URLs are never logged.

State input, stream backlog, active streams and returned actions are bounded.
Frames share one sequence and reconnect receives only the current presentation.
Revocation is serialized with authorization and dispatch. Shutdown or input closure
cancels delivery. UI actions are intents only; this library executes no tools.

Build the existing page before serving its JavaScript bundle. Missing bundles return
503. TLS with a pinned certificate must precede LAN exposure in W3. This component
does not change the config default or expose the bridge on the LAN.
