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

The retained S5 fixture now launches the already compiled Rust integration test
as its client. That client uses the public supervisor, bounded IO, selected wire
types and ThreadSettings against the unmodified pin. Python owns only the local
HTTP/WebSocket/Responses fake and its exact wire-value oracle. A pipe readiness
signal confirms receipt of the delegation append before Rust stops the call;
shutdown awaits child reaping. No compilation occurs inside that journey.
Controlled-process T3 tests separately exercise initialization, crash/restart,
drop, pipe failure and isolated-session boundaries. Real-pin fault cases cover call creation, sideband handshake and delegated-model
refusal. The real-pin concurrent drop witness holds both sessions at a tool
request before dropping one supervisor. An OS exit observer and the fake's
accepted-socket inventory establish cleanup before releasing the survivor to
complete its exact tool/append/close journey. Each session owns a distinct fake
port, home and working directory; pipe barriers also work across separate Linux
network namespaces. Linux confinement remains a required platform witness.

Initialization cancellation and timeout tests hold the child's handshake at a
loopback readiness barrier. Cancellation keeps the child owned and IO private;
restart kills the held generation before exposing a newly initialized one.
Timeout advances the client clock, then resumes real time for OS reaping.
The offline helper CLI test copies an already provisioned cache into a private
temporary path with spaces, rejects missing/corrupt artifacts, and exercises
competing helpers with an independently held writer lease. Provisioning remains
outside tests; a cache miss cannot download during this journey.

The public-client witness runs independent real-pin journeys concurrently after
one offline helper resolution. Each journey owns its home, working directory,
loopback server and namespace. The successful path checks user and assistant
transcript deltas and final text. An interrupted path holds the incoming tool
request, sends the matching thread/turn interrupt and withholds its tool result.
Call and provider refusals preserve the pinned error notifications and closed
reason. The fake asserts exact retry counts and absence of successful tool
results, and waits for all accepted sockets to close after awaited shutdown.
A journey watchdog bounds failures while retaining supervisor cleanup; it is a
ceiling for the unmodified process, not a latency requirement.

The later bridge cutover owns the tool host, MCP, caps and milestone scenarios.
This component does not claim M2, M4, real authentication, media or hardware
proof. Reverting this additive lane removes its crate, registration and helper
provisioning; there is no product state migration.

## Private-home reopen amendment (2026-11-17)

The coordinator's binding mailbox option A authorizes the bridge-cutover lane
to reopen a retained home, enabling separate sign-in and bridge processes to
use the same app-server state. This supersedes the fresh-only restriction
above. It does not authorize copying credentials, importing another home,
changing shared contracts or adding a dependency.

The existing `.echo-codex-owner` file is the ownership mechanism. A fresh home
creates it with `create_new`; a retained home opens that same regular file for
read/write without creation or truncation. `File::try_lock` takes an exclusive,
nonblocking lock. The returned file stays in `Supervisor._lease` throughout
shutdown and restart until supervisor drop. A second owner fails with the
existing payload-free `ProcessError::PrivateHome`. Normal shutdown reaps the
child before the owner drops. Abrupt owner death with a surviving child is
outside this amendment's proof; callers use awaited shutdown.

Preparation rejects relative, root, parent-traversing and redirected paths,
repository `.git` directories and worktree pointers in every ancestor,
unmarked retained homes, nonregular markers and redirected working directories.
It canonicalizes the parent and checks both the supplied and resolved ancestor
chains without invoking Git or depending on the source checkout. Existing work
directories and marker files are retained. Preparation never opens, writes,
copies or deletes `auth.json`.

Unix homes require mode 0700. On Windows, inherited ACLs must be verifiable
through the built-in, noninteractive PowerShell `Get-Acl` command. The owner
and every allowed identity must be the current user, SYSTEM or Administrators.
The script and argv are fixed; the target path is passed as environment data.
Output is discarded and unverifiable privacy fails closed. The helper removes
an inherited `PSModulePath` so PowerShell loads its own built-in security
module. It does not modify production ACLs. Controlled-process fixtures create
private parent directories to avoid depending on the host temp directory's ACL.

The public supervisor tests exercise clean reopen with byte-identical synthetic
authorization and retained marker/work bytes, rejection while the live owner
or a shut-down supervisor retains its lease, admission after drop, and the
existing fresh-home and unsafe-path cases. Platform cases reject public Unix
permissions, symlink redirection and public Windows ACLs. These are local
controlled-process checks, not real sign-in or milestone proof.

## Bridge account adapter amendment (2026-11-17)

The cutover's `echo-signin` account adapter consumes the existing `echo-codex`
bounded IO and account schema. This adds an internal workspace dependency edge
and promotes already locked `serde_json` from test use to runtime use; it adds
no third-party package. Legacy auth APIs remain dormant until their callers
are migrated in the retained cutover story; this increment does not claim that
the bridge CLI or the voice path has switched.

Sign-in uses `account/login/start` and requires a successful
`account/login/completed` with the matching login ID. Account updates and
foreign completions cannot report success. The adapter presents the same
device URL/code or browser URL to its writer and host-owned screen callback,
with no invented expiry. An absolute host deadline bounds the entire operation;
failure closes the IO generation, after which the host awaits supervisor
shutdown. The host also owns cleanup when cancelling the command future.
The API-key path serializes only a nominated `Secret` through account login
and confirms the resulting account mode. ChatGPT account checks never acquire
a bridge-managed token. Status uses `account/read` without refresh and masks
account metadata entirely because the pin does not expose the former account
ID or expiry fields. The legacy import command is refused by this adapter.

Controlled JSONL peer checks exercise correlation, deadlines, public prompt
output, malformed input, writer/screen failures, account status and API-key
confirmation. A controlled stdio child exercises the actual supervisor and
adapter, awaited shutdown, exclusive ownership and a new process reopening
byte-identical synthetic auth. These protect the consumer boundary; they do
not prove provider redemption, the bridge CLI, screen transport or M2.
