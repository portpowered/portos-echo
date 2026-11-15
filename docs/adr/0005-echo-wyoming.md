---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted; Wyoming client dependencies
---

# Bounded asynchronous Wyoming connection

The bridge needs simultaneous microphone reception and reply playback over one
Wyoming connection. The `echo-wyoming` library uses `echo-contracts` for every
wire event, framing decoder and encoder. No shared contract changes are needed.

Use `tokio` for TCP, bounded channels, task ownership and operation deadlines,
as required by the W1 packet. Its `tokio-macros` package provides async tests and
selection. The newly resolved transport closure contains `bytes`, `mio`,
`socket2` and `wasi`. These packages provide buffered I/O and platform socket
readiness; none adds a service or a credential provider. Existing `serde_json`
and `thiserror` dependencies remain the JSON and fixed error-category boundary.

The initial connection spine registers explicitly, receives typed run handles
without waiting for a microphone audio-start, and acknowledges reply completion
only after played. A dedicated reader prevents command selection from cancelling
partially received frames. Frame sizes and channel lengths are bounded; a slow
microphone consumer loses the oldest frame and retains per-run drop counts.
Lifecycle overflow terminates the connection rather than losing control events.

The final story in the same lane adds automatic reconnect. It must preserve
the single-owner playback acknowledgment boundary
and must never replay an old generation's reply audio. Real host and milestone
evidence remain separate from controlled peer evidence.

Story 002 uses the dev-only `loom` model checker required by the queue-concurrency
plan. The model uses the production queue state and eviction algorithm under a
modeled mutex to explore insertion, receiving and close interleavings. It does
not claim to model Tokio notification or OS socket scheduling.

Its generator and tracing dependency closure adds `generator`, `cc`,
`find-msvc-tools`, `shlex`, `log`, `matchers`, `nu-ansi-term`, `scoped-tls`,
`sharded-slab`, `thread_local`, `tracing`, `tracing-core`, `tracing-log`,
`tracing-subscriber`, `valuable`, `windows-core`, `windows-implement`,
`windows-interface`, `windows-link`, `windows-result` and `windows-strings`.
These packages support deterministic test scheduling, native stack switching and
test diagnostics; none joins the normal runtime dependency graph.

Reply ingress uses the same oldest-evicting queue with a separate bounded pump.
Cut closes that source, discards unsent frames and sends an early audio-stop;
completion still waits for played. Pause ends capture until explicit registration
and does not cancel playback. Dropping an active playback future invalidates the
connection so an uncorrelated acknowledgment cannot complete another reply.
