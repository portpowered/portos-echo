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

The supervisor creates a fresh external CODEX_HOME and separate empty working
directory before starting a child. Existing homes and repository paths are
rejected; the caller retains responsibility for deleting or retaining the home.
Unix homes are created with mode 0700; Windows uses the caller's inherited
directory ACL. The child receives an allowlisted system environment, with HOME
and USERPROFILE redirected to its dedicated home. Proxy/certificate settings
may be supplied explicitly before launch; provider credentials use account RPC.
Config overrides are individual argv entries, never shell input. Stderr is
discarded so payloads cannot enter diagnostics or block an undrained pipe.

Each generation must acknowledge experimental initialization and flush the
initialized notification before IO becomes accessible. The caller drives IO and
explicitly shuts down or restarts after a terminal failure. Restarts kill and
await the old child, consume a rolling monotonic budget (two in 60 seconds by
default), and create fresh request state without replay. Failed launches consume
the budget too. Cancellation of launch leaves its child owned but unavailable
until restart/shutdown. Explicit shutdown awaits reaping within the configured
timeout. Drop signals termination; Tokio supplies best-effort asynchronous reap,
so callers needing an awaited cleanup boundary use shutdown before runtime exit.

The pin helpers, settings and raw-stdio S5 witness preserve the initial executable
spine. Controlled-process T3 tests exercise the supervisor's initialization,
crash/restart, drop, pipe failure and isolated-session boundaries. The final
public-client pinned-server T3 remains required in this same work item. These
process witnesses do not establish real app-server call behavior through Rust.

The later bridge cutover owns the tool host, MCP, caps and milestone scenarios.
This component does not claim M2, M4, real authentication, media or hardware
proof. Reverting this additive lane removes its crate, registration and helper
provisioning; there is no product state migration.
