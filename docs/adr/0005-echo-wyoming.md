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

Subsequent stories in the same lane add cancellation controls and automatic
reconnect. They must preserve the single-owner playback acknowledgment boundary
and must never replay an old generation's reply audio. Real host and milestone
evidence remain separate from controlled peer evidence.
