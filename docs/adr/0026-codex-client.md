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

## Operator CLI cutover amendment (2026-11-17)

The canonical `echo-bridge signin` and `status` commands now host that account
adapter in an exclusively owned supervisor. This adds the existing workspace
`echo-codex` dependency to `echo-bridge`; no third-party package is added.
Configuration parsing stays offline. Active account commands require
`codex.home`; an omitted binary resolves through the existing platform helper
with `ECHO_CODEX_OFFLINE=1`, so provisioning is a separate operation. Fixed
home/binary diagnostics exit 2 without disclosing a path. The helper has a
bounded runtime and cannot download during startup. A configured executable
is checked before home preparation and must be executable on Unix.

Sign-in owns the existing paired screen while the bridge is stopped and uses
`Ready.message` for the exact public URL/code shown in the CLI. No expiry or
screen contract is invented. Status acquires the same exclusive home and
reads account metadata through the app server, without opening the bridge's
legacy credential file. `import-codex` exits 2 with instructions to sign in;
its source is never read. Kill commands update the existing kill file without
resolving a binary or taking home ownership. Account failure, output failure
and explicit host cancellation await child and screen cleanup before releasing
the lease. Abrupt process termination retains the supervisor's existing
kill-on-drop fallback; this amendment adds no OS signal handler.

The prebuilt CLI tests use a controlled stdio account child and real loopback
screen transport. They protect device/browser prompts, foreign completion
rejection, successful matching completion, retained synthetic auth, sanitized
failure/EOF, held-home exclusion, import refusal, missing configuration/binary,
offline cache failure, output failure and host cancellation. These replace the
CLI's former provider redemption/callback tests; legacy OAuth component tests
remain in `echo-signin`. While the voice runtime migration is retained work,
its test fixtures call the legacy library explicitly rather than claiming that
the canonical CLI still imports or redeems those credentials. Voice calls,
real-pin M2, browser rendering and remote authentication remain separate gates.

## Bridge call signaling amendment (2026-11-17)

The bridge's call-control adapter exclusively borrows one initialized
`echo-codex` IO generation. It uses the existing pinned `ThreadSettings` and
`RealtimeStart` serializers, adding the bridge's optional delegated model,
instructions and voice. Realtime start always selects V3 WebRTC. The media
owner supplies its offer and receives only the matching thread's SDP answer;
the signaling adapter does not infer media connectivity from that answer.
This adds no dependency, copied upstream implementation or protocol change.
The pin, rather than the research brief's older main snapshot, defines the
wire shapes (brief sections 2, 3 and 5; program plan section 3.3).

The adapter preserves early transcript/tool events in a bounded queue while
waiting for SDP. Only the matching assistant `transcript/done` signals the
reply's end; draining accepted media and receiving `played` remain the run
owner's responsibility. An acknowledged stop also requires the matching
`thread/realtime/closed`. A failed or abandoned start/stop closes the IO
generation; the host must still await supervisor shutdown before releasing
ownership. A successful stop leaves the initialized generation usable for a
later call. Unknown or foreign server requests receive unsuccessful results
without executing a tool. Matching requests retain their original IDs for the
future authorized tool dispatcher.

Component tests exercise memory-stream JSONL and a paused clock, including
early notifications, foreign IDs, exact settings, malformed responses,
overflow, deadline, EOF and cancellation. They do not prove the real app
server, ICE, PCM, screen rendering or any milestone. The voice runtime still
uses the retained legacy path until the planned supervisor/media/simulator
wiring is complete; this adapter alone does not satisfy the cutover.

## Bridge media ownership amendment (2026-11-17)

The bridge adds an internal dependency on the existing `echo-media` workspace
crate. No third-party package, media pin or shared protocol changes. Its joint
call owner obtains the peer's offer, sends it through the signaling adapter,
applies the matching answer and requires the peer's connected state before
returning success. Offer creation, signaling, answer application and ICE/DTLS
readiness share one absolute deadline. Signaling continues during media
negotiation; early transcript/tool events retain a bounded FIFO rather than
blocking the process pipes. Overflow and failure close the generation.

Microphone input is the media crate's complete 20 ms mono/16 kHz PCM frame;
Opus and 48 kHz conversion remain in `echo-media`. The owner fairly polls
decoded PCM and app-server events without adding a media queue. Assistant
transcript completion remains a separate event and does not discard accepted
audio. The future run owner must convert replies to Wyoming PCM, pace them,
drain accepted media and await `played`. Closing awaits both signaling stop
and media resource release, including when one fails. Host cancellation must
still await cleanup and supervisor shutdown; drop retains the existing peer
and signaling cancellation fallbacks.

Memory-only component tests use controlled media/signaling boundaries and a
paused clock for readiness, correlation, deadlines, overflow, EOF, tool-result
delivery and cleanup. A T3 component check connects the real `echo-media` peer
to the existing `echo-live::LivePeer` used by local fakes, with real SDP,
ICE/DTLS/SRTP and a nonzero PCM round trip. All sockets bind IPv4 loopback;
protocol readiness drives the handshake, without fixed sleeps. This proves
interoperability with the planned local fake's peer; it does not execute the
real app server or canonical voice runtime, measure pacing or prove M2.

## Bridge PCM conversion amendment (2026-11-17)

The bridge's run-facing audio adapter converts bounded Wyoming PCM chunks to
complete mono/16 kHz media frames without Opus encoding. It preserves sample
order across irregular chunks, downmixes supported stereo input through the
existing PCM converter and reports an unpadded microphone tail. Format changes,
incomplete interleaved samples and post-finish input fail closed. A new owner
after mute or cut discards filter history and partial samples.

Replies convert decoded media PCM from 16 kHz to Wyoming's existing 24 kHz
format. Complete 20 ms frames enter an oldest-evicting queue of 128 frames,
with explicit cumulative eviction accounting. The adapter shares the existing
run pacer: configured initial lead (200 ms by default), then one frame per
20 ms, without catch-up bursts or renewed credit after underflow. Cancelling
a paced wait consumes no frame. Finishing drains filter delay once; a drained
buffer proves only handoff of retained PCM, never a Wyoming played receipt.
The caller must first establish that accepted media has drained; an assistant
transcript completion alone does not authorize finish.

This adds no dependency, shared contract or codec path. Memory-only tests use
a paused clock to assert exact framing, sample duration, tail handling,
validation, queue evictions and pacing. The existing T3 media component now
sends irregular Wyoming PCM chunks through capture conversion and checks
nonzero reply PCM, exactly three 960-byte Wyoming frames, and zero drops
through the real media peer. This does not switch canonical voice startup,
establish the end of a remote reply, await Wyoming played, or prove M2.

## Bridge authenticated host amendment (2026-11-17)

The bridge host boundary owns one supervisor through initialization,
authentication and an exclusively borrowed session. ChatGPT admission uses
the app server's account state; API-key admission uses only a caller-nominated
`Secret` through `account/login/start`, followed by matching account mode
confirmation. Admission errors never invoke session work. Startup and account
admission share an absolute deadline; the admitted conversation has its own
lifetime. The boundary never publishes readiness or Connected for the caller.

Operator commands share the same startup/cancellation/reaping owner. Normal
completion, callback failure and cancellation await supervisor shutdown before
releasing its private home lease. A callback still owns its media and call
cleanup: cancellation drops that borrower before process shutdown, so callers
needing graceful media stop must settle it inside their session work.

T3 tests use the actual supervisor and controlled Python stdio process, with
private external homes, allocated loopback control ports and readiness signals.
They assert exclusive ownership, one initialized child, account-mode gating,
nominated API-key delivery, unchanged synthetic auth bytes, callback failure,
cancellation during initialization/account/session work, process socket release
and lease reacquisition. No provider or hardware is used. This boundary is not
yet selected by canonical voice startup and does not prove real-pin M2.

## Bridge device run amendment (2026-11-17)

The run owner joins the existing call/media owner with Wyoming capture and
playback. It creates no additional process or IO reader. Capture conversion,
bounded reply conversion and the shared lead pacer remain separate component
boundaries. During paced waits, blocked playback sends and played settlement,
the owner continues reading app-server control and device events. One pending
PCM frame survives cancellation of a blocked send; its successful handoff is
counted once. The host supplies observation callbacks for screen and trace.

Only assistant transcript completion starts local media drain. A nonblocking
receive drains already accepted PCM in order before flushing conversion. This
is a local queue boundary, not proof of remote audio EOF: later PCM fails the
run rather than silently truncating a reply. Real-pin M2 must still establish
that the provider's transcript/media ordering supports the complete reply.
Neither a user transcript nor an unrecognized event ends playback. Success
requires drained retained PCM, source close, the device's played event, the
playback operation's Played result and capture completion. Empty replies also
await played. Response and played settlement have independent deadlines with
one overall ceiling. A premature receipt, media EOF or error cannot publish
successful completion.

Mute suppresses capture and discards partial capture state. Cancellation,
transport, observer and deadline failures request a bounded cut; the outer
conversation owner must await call/media and device cleanup before reuse or
supervisor shutdown. Unhandled tool requests fail without an effect until the
authorized dispatcher is connected. This component does not implement M3.

Paused-clock memory-only checks protect completion, exact PCM and counters,
200 ms initial lead, oldest-frame evictions, control under backpressure,
capture admission, error cleanup and missing/premature played. A combined
component check uses actual MediaCall JSONL projection and the in-memory device
port with controlled media. These checks do not select canonical voice startup
or prove pinned app-server ordering, real Wyoming playback or any milestone.

## Bridge conversation lifetime amendment (2026-11-17)

The device conversation owner retains one connected `MediaCall` across played
replies and subsequent wakes. Each completed reply renews the configured idle
deadline. Expiry takes precedence over a queued wake at equality; a muted wake
does not renew it. An unrepresentable idle deadline never expires, preserving
the existing configuration behavior. Response and played durations must fit
the monotonic clock before admission. Idle media belongs to the previous reply
and is discarded before the next run; it is never replayed into later playback.

The owner keeps polling the app-server and media while waiting for a wake.
Device and control reads are fair; cancellation and expiry take precedence.
Every return awaits bounded call stop and media cleanup, including observer
failure, device/control EOF and cancellation during playback or idle. Automatic
media cleanup on signaling failure marks the owner closed, preventing duplicate
peer teardown during outer cleanup. The host still owns device close and child
reaping. Cancellation must reach this owner inside the authenticated host
callback when graceful signaling stop is required.

Paused-clock component tests use memory-only JSONL, media and device ports to
protect reuse, deadline renewal/equality, muted wakes, old-PCM exclusion,
observer failure, cancellation and process EOF. These tests do not establish
canonical CLI selection, kill-file observation, actual app-server/media or
Wyoming behavior, real-pin M2 or any milestone. Those remain retained work.
