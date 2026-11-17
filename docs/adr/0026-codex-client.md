---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: implementation in progress
---

# Pinned Codex app-server client

The [program plan](../program/plan.md#33-the-codex-app-server) selects the
unmodified Codex app server for call control and delegated turns. `echo-codex`
is an additive component; the bridge and its current Live path remain unchanged.
The selected release is `rust-v0.160.0`, commit
`a956835d020762cb2b570053af06f643a11c0ecc`. `codex.pin` fixes the Linux musl and
Windows asset digests. Both helpers verify archives before publishing an external
cache, retain upstream LICENSE and NOTICE, and reject corrupt warm entries.
`ECHO_CODEX_OFFLINE=1` permits only cache reads during a test.

The crate uses the workspace's existing `serde`, `serde_json`, `tokio` and
`echo-contracts::Secret` dependencies. No new third-party package, upstream
Rust crate, FFI or shared contract is added.
Selected settings are adapted from schema generated offline by the verified
release. The full schema and the binary remain outside git. Attribution is in
the crate's NOTICE and adapted source; upstream is Apache-2.0.

The [S5 local fake](../spikes/codex-local-fake.md) establishes that config-only
redirects work for the call, sideband and Responses model. Empty environments
remove apply_patch; shell flags alone do not remove all built-ins. Explicitly
disable multi-agent, goals and request-user-input tools as well. Preserve the
caller tool list exactly, including an empty list. The settings builder uses
ephemeral threads, read-only sandbox and approval policy never.

The crate's IO-independent request core uses monotonic elapsed time supplied by
its caller. It bounds physical JSONL bytes, admitted requests (including unread
completions), server requests awaiting answers and unsolicited events. Overflow
closes the connection, fails pending requests and leaves accepted events
drainable. Cancellation releases admission without reusing IDs; late replies
cannot satisfy another request. Remote errors retain only their numeric code in
diagnostics. Unknown notifications retain their complete JSON envelope.

The async stream driver services reads, writes and monotonic deadlines when the
caller polls it. It owns no background tasks. Both input fragments and output
offsets survive cancellation of a wait future; abandoning a request requires
explicit cancellation of its ID. Wholly unsent cancelled/expired calls are
discarded. A partially written frame must finish to preserve framing, but its
late reply cannot satisfy another waiter. Output frames, including notifications
and tool replies, have a separate queue capped by the pending-request limit.
Stalled writes fail the generation within the configured request timeout and
close both stream halves. Event overflow preserves previously accepted events.

Selected wire types come from the pinned experimental schema, including
account/device-code sign-in and known realtime notifications. Credentials use
`Secret`; payload-bearing reply/notification types have no Debug or Display.
Unknown notifications remain available as complete raw envelopes regardless of
whether a caller chooses to deserialize a known projection.

The pin helpers, settings and raw-stdio S5 witness preserve the initial executable
spine. The supervisor, process fault matrix and final public-client T3 witness
remain required in this same work item. The network-free stream tests establish
IO bounds and scheduling, not child lifecycle or the Rust-driven call journey.

The later bridge cutover owns the tool host, MCP, caps and milestone scenarios.
This component does not claim M2, M4, real authentication, media or hardware
proof. Reverting this additive lane removes its crate, registration and helper
provisioning; there is no product state migration.
