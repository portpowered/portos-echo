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

The crate uses the workspace's existing `serde` and `serde_json` packages. No
new third-party package, upstream Rust crate, FFI or shared contract is added.
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

The pin helpers, settings and raw-stdio S5 witness preserve the initial executable
spine. Selected wire types come from the pinned experimental schema. The Rust IO
driver and supervisor, account/notification projections, process fault matrix and
final public-client T3 witness remain required in this same work item. S5 does
not establish the Rust client's process lifecycle or runtime backpressure.

The later bridge cutover owns the tool host, MCP, caps and milestone scenarios.
This component does not claim M2, M4, real authentication, media or hardware
proof. Reverting this additive lane removes its crate, registration and helper
provisioning; there is no product state migration.
