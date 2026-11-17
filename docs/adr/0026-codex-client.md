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

## Authenticated device ownership amendment (2026-11-17)

The voice host composes the existing authenticated supervisor with the device
connector. Initialization and account admission precede connection, capture and
probe; only then may session work borrow the sole initialized IO generation and
device. Device readiness still cannot establish Connected: media negotiation
must succeed first. One absolute deadline bounds process/account/device startup.
No legacy call or credential path is invoked by this owner.

Cancellation has two stages. Before account admission, it interrupts startup.
After admission, a watch receiver informs session work, allowing the conversation
to await signaling/media stop while its IO and private home lease remain owned.
The session callback must observe this receiver and clean up its calls on every
return. The host then awaits device close under a two-second ceiling and awaits
supervisor shutdown before releasing the home. Device startup or callback failure
also closes the device; the original operational error takes precedence over a
secondary device cleanup error. The outer owner must be awaited through shutdown.

Controlled-process tests protect account-before-connect ordering, no dial on
authentication/binary failure, initialized IO and exclusive ownership during
callback cleanup, device close and child reaping. Memory-only paused-clock tests
protect cancellation and the absolute connection deadline. These do not prove
actual Wyoming startup, media stop ordering, screen state or real-pin M2. The
canonical executable still requires migration to this host and the conversation
owner; existing runtime selection remains unchanged in this increment.

## Conversation admission amendment (2026-11-17)

The voice owner now composes media negotiation with the existing conversation
on the authenticated host's exclusive IO borrow. It emits Connected only after
both the matching signaling answer and media readiness. An expired startup
deadline, ready cancellation or invalid response/played budget is refused before
an offer or RPC. Startup and ICE share one absolute deadline; cancellation wins
when ready at admission. Early transcript notifications remain queued until
media connects and the run owner can consume them.

Every return awaits call/media cleanup, including a cancelled start or failure
to publish Connected. Operational failures retain priority over cleanup errors.
A cancelled or failed negotiation closes the IO generation without replay;
successful idle closure preserves initialized IO. Device close and supervisor
reaping remain the authenticated host's responsibility. This adds no process,
reader, credential path, dependency or shared contract.

Paused-clock, memory-only component checks protect readiness ordering, accepted
early transcript delivery, actual played settlement, pre-admission refusal,
ICE failure/deadline/cancellation and observer failure cleanup. They do not
establish canonical executable selection, UI rendering, kill/relink behavior,
actual app-server/Wyoming interaction or real-pin M2. Those remain retained work.

## First-wake admission amendment (2026-11-17)

The voice service takes the device event stream once inside the authenticated
host callback. Before the first wake and between idle-closed conversations it
polls the same process IO, refuses unsolicited server requests without tool
effects, observes ordered device mute/drop events and probes device readiness.
One in-flight probe survives unrelated events; each probe has a five-second
ceiling and the next starts after a one-second cadence. Failure ends this
borrower; transport recovery remains the device adapter's responsibility.

Only an unmuted wake admits media creation and starts the connection deadline.
Time spent ready before a wake does not consume that budget. Connected still
requires the existing signaling and media admission, and each played reply
retains its call until idle expiry. After awaited stop, the service returns to
Ready and can admit another call on the same authenticated IO. Mute state is
retained across these phases. Cancellation reaches active conversation cleanup
before the outer host closes the device and reaps its child.

Memory-only paused-clock tests protect unsolicited-request refusal with the
original ID, device probing, mute/drop order, EOF/cancellation, delayed first
wake, two complete conversations on one IO owner, and peer-creation deadline
and cancellation. No process or socket is created by these tests. Canonical
executable selection, UI/relink/kill projection and real-pin M2 remain retained
work; this component proof does not satisfy those edges. No new dependency,
shared contract or credential path is introduced.

## Voice presentation lifetime amendment (2026-11-17)

The bridge voice library composes its authenticated device host and first-wake
service with the existing screen, health and trace owners. Presentation remains
owned until call/media stop, device close and child reaping finish. Failed
authorization publishes the existing sign-in instruction as an overlay, retains
the screen and health endpoint until host cancellation, and never dials the
device or attempts implicit sign-in. The overlay survives mute and unmute. The
reaped process no longer holds its home lease while that instruction is shown.

Only the service's negotiated-media observation publishes Connected. Initial
Listening precedes negotiation; subsequent wakes within a retained call publish
Listening without another Connected. Transcript completion and the played
notice alone do not publish Ready: the run owner must also report completed
capture, accepted-media drain and both played witnesses. Completion emits run
counters and Ready; idle stop emits one live.closed. Traces omit transcript
bodies and all provider payloads. Publication errors prevent completion claims.

Component tests exercise that projection through its bounded state queue and
trace output. Controlled-process T3 tests exercise authenticated readiness,
paired screen state, cancellation/reaping, persistent relink and missing-binary
refusal without a wake. These are library composition witnesses, not canonical
CLI or real-pin M2 evidence. Executable selection, kill-file policy, delegated
tools, per-chunk mute accounting and real-pin transcript/media ordering remain
retained work before the cutover can pass. No dependency or contract changes.

## Voice kill ownership amendment (2026-11-17)

The Codex voice service observes the same durable kill marker as the operator
command. A missing marker permits work; an empty regular marker or an
unverifiable/invalid marker refuses work. Read before admitting every wake and
poll while idle, creating a peer, negotiating media, capturing, playing and
retaining a call. Refused wakes produce the existing Wyoming kill reason and
never create a peer or a thread. This policy reads no credential or home file.

Kill cancellation retains the playback future through a bounded cut and device
played receipt, then awaits realtime stop and media cleanup. Only successful
cleanup and an intact IO generation allow return to the idle service. Failed
played settlement, stop acknowledgement or media cleanup terminates the owner;
clearing the marker cannot reuse that generation. Successful clear permits a
fresh call on the same supervisor and device connection, without replaying the
interrupted run. Record an observed kill even if the marker clears while
cleanup is pending. Health and the existing screen overlay retain the kill
state across Ready and mute/unmute, and clear restores idle presentation.

Memory-only signaling/media/device tests with paused time protect refusal,
active interruption, exact stop/close counts, clear/reuse and missing-stop
failure. Component projection tests protect health, trace and overlay values.
The controlled-process/paired-screen witness protects prewake kill and clear
without creating media. These do not prove canonical CLI selection or real-pin
M2, which remain required before cutover acceptance. No new dependency or
shared contract is introduced.

## Configured voice admission amendment (2026-11-17)

The voice configuration adapter owns its validated bridge config, audit kill
path and trace path for the entire authenticated service lifetime. Preparing it
requires a Codex home and the client backend, without opening credentials,
resolving a binary or creating a home. Runtime reads only the nominated API-key
file in API-key mode, bounds it to 16 KiB and wraps the validated value in
`Secret`. ChatGPT mode never opens the legacy credential path. Process login
still owns authentication, and device readiness still follows account admission.

Effective Codex voice and delegated model use the existing config precedence.
When neither voice is supplied, omit the realtime voice field so the pinned
server chooses its default. Do not substitute a legacy voice or send null.
Preserve configured instructions, idle interval, response deadline and reply
lead; retain the existing kill marker and caller-owned dynamic tool list.

Memory-only checks protect credential bounds, offline preparation, effective
options and the exact voice-omitted start request. Controlled-process checks
exercise both authentication modes through this adapter, paired readiness,
missing-binary/key refusal and awaited cancellation cleanup. The executable
selector and scenario consumers have not switched to this adapter yet; real-pin
M2 and tool dispatch remain required. No shared contract or dependency changes.

## Executable voice selection amendment (2026-11-17)

Ordinary configured client runs now select the authenticated Codex voice owner.
Offline input retains the validated configuration without constructing a legacy
credential provider. The executable supplies the production echo-media factory;
account admission precedes Wyoming capture/probe and Ready. Missing home or
binary is a fixed configuration error with exit code 2 before device admission.
Stdin EOF requests awaited call/device/process cleanup. Invalid stdin control
remains an operational failure. Ready and completed runs retain the existing
stdout notifications as well as the trace observations.

This increment does not complete the cutover. Configured MCP servers fail
closed until the dynamic-tool dispatcher is integrated. A nominated Codex home
cannot enter the retained legacy simulator: loopback and fake-clock runs with
that home are rejected before runtime effects until their process-local routing
is migrated. Legacy simulator fixtures without a Codex home remain available
for their protecting tests; Muse selection is unchanged. These temporary guards
must be removed by completing the retained integration, never by falling back
to legacy credentials, calls or delegated turns for a configured Codex home.

Prebuilt executable witnesses use a controlled stdio account child and loopback
Wyoming peer. They protect both auth modes, one process before Ready, EOF/reap,
missing binary/home, relink without device admission and refused unmigrated
tools/simulation. They do not prove a real pinned call, delegated tools or any
milestone scenario. Those proofs remain required before acceptance. No pin,
dependency or shared protocol changes.

### Process-local simulation routing

Configured runs with `--live-loopback` and `--model-loopback` now
carry numeric loopback origins into the same authenticated Codex voice owner.
The child receives call and sideband base overrides, a local Responses provider,
and the S5 probe's discovery/tool/telemetry restrictions. Proxy variables in
both spellings are set only through Supervisor's existing environment API;
no parent environment mutation, inherited provider key or new credential path
is used. Ordinary runs receive none of these overrides. Codex simulation no
longer constructs a legacy credential provider during input preparation.

The pinned S5 fixture (`crates/echo-codex/tests/support/codex_probe.py`) is the
source of these routing settings; this does not move the pin or edit Codex.
Controlled prebuilt-executable witnesses inspect effective child argv as parsed
TOML, exact proxy settings, account-before-Ready and EOF/reap for both auth modes.
The existing explicit Tokio clock driver selects this same owner and acknowledges
clock advances; it does not enter the legacy simulator for a nominated home.
These witnesses do not establish network confinement, a real pinned call or
idle/reply timing through that pin. The local V3/media fake and canonical M2
remain required integration work. Configured tools and simulated daily epochs
still fail closed pending their retained dispatcher and budget integration.

### Pinned signaling and real media component witness

The bridge's `bridge_codex_media` T3 suite now also runs the verified offline
Codex pin through `with_host_options` and `MediaCall`. A stdlib Python provider
answers the call HTTP request with actual SDP from the existing loopback
`echo-live::LivePeer`; the bridge uses production `echo-media`. The provider
keeps the V3 sideband open and emits user and assistant transcript events only
after the PCM round trip. The test checks the role/delta/done sequence, awaits
realtime stop and supervisor reaping, then reacquires the same private home.
The invoking Cargo build owns the test artifact; execution never downloads or
builds the app server. The existing test-unit job provisions the verified pin.

The pin's observed default call model is `gpt-live-1-codex`, whereas the research
brief's main-branch default is `gpt-live-1.1-codex`; the pin wins. The witness
checks the V3 `quicksilver=v2` header and exact call query. Process-local proxies
refuse unexpected provider paths. This is routing evidence, not OS network
confinement. It proves the signaling/media boundary with API-key RPC login
using an inert local key; it does not prove ChatGPT authentication, Wyoming
playback settlement, cross-channel reply ordering, idle reuse, or the M2
scenario. Those retained obligations remain unsatisfied.

### Pinned executable voice witness

Explicit numeric loopback call/model routes now select a media peer restricted
only to IPv4 loopback, with loopback candidates enabled. Ordinary configured
calls retain the media peer's LAN defaults. No STUN endpoint or new dependency
is introduced. The executable heap-allocates the peer factory and each admitted
conversation future; the Windows main thread otherwise overflows its stack on
the first real wake even though startup and component media tests pass.

The prebuilt executable witness in `bridge_codex_media` owns a private external
home, the verified offline app-server pin, local Python provider, real echo-media
and an existing LivePeer remote, plus a local Wyoming socket peer. It checks
loopback-only offer candidates, one native V3 call/sideband, nonzero 24 kHz reply
PCM, no completion before `played`, completion after acknowledgment, EOF cleanup
and exclusive home ownership until the executable exits. Time advances through
the existing explicit clock controls on capture and remote protocol progress;
partially received Wyoming frames survive those controls. It never builds an
artifact inside the test or uses a real account. This is an executable T3
interoperability witness, not the hassmic-host M2 scenario, ChatGPT invalid/expired
proof, direct browser verification or OS-enforced network confinement. Those
retained gates remain unsatisfied.

### M2 runner provider ownership

M2's success case now configures the delivered bridge with a fresh private
external Codex home and the verified offline pin. Its stdlib Python provider
owns call HTTP and the V3 sideband; a single Rust owner answers with real SDP
from the existing local LivePeer and echoes Opus. The bridge uses echo-media.
The runner advances the explicit bridge clock on remote protocol progress,
releases assistant transcript completion only after Speaking, and waits for
the bridge's played settlement before driving idle closure. The remote media
driver quiesces before bridge shutdown and retains its socket until cleanup.
Provider shutdown before an offer also has an explicit joined ownership path.

Success uses only the nominated inert API-key login. Its report identifies
that mode; it does not establish ChatGPT credential acceptance or refresh.
The two credential-negative cases still require migration from the legacy
fixture. The exact success screen sequence gains Speaking and the final Ready
after idle closure, preserving both intervening Ready publications. The full
M2 milestone remains gated on the native host scenario and both negatives.

### Local refresh routing amendment (2026-11-17)

The binding coordinator answer at 15:11Z additionally admits one process-local
`CODEX_REFRESH_TOKEN_URL_OVERRIDE` through `Supervisor::set_environment` before
first launch. It accepts only an explicit nonzero port on `http://127.0.0.1` or
`http://[::1]`, with the exact `/token` path. Duplicate settings, external hosts,
other schemes, paths, queries and fragments fail closed. The existing cleared
child environment remains in place. No credential is passed through this key.

Only bridge-owned `LocalRoutes::environment` supplies this setting, pointing at
the nominated fake Live origin. Ordinary configured launches never set it, even
when the parent environment contains it. No production configuration field,
bridge refresh client, shared contract or dependency is added.

The component fixture provisions its private home through the public supervisor
and writes only inert synthetic auth in the pin's existing store representation.
It exercises actual pinned-server account rejection and the local refresh
endpoint, checks no session admission or call/sideband/external request, retains
identical synthetic auth bytes and reacquires ownership after cleanup. This is
component evidence; it does not migrate or satisfy the M2 scenario's negative
cases, exact refresh counter, screen sequence or post-rejection wake behavior.
Those scenario obligations remain open.

### Canonical local ChatGPT admission

A configured client with a Codex home no longer validates the unused legacy
ChatGPT refresh endpoint when admitting paired local provider origins. Its
refresh routing remains exclusively `LocalRoutes::environment`; the legacy
endpoint guard remains for retained legacy and Muse paths. Wyoming, screen,
MCP and both provider origins retain their loopback checks.

Prebuilt executable tests now exercise malformed and expired synthetic homes
through the actual pin. They assert the exact relink frame, no prior Ready or
Listening publication, no Wyoming admission, no call/media effects, sanitized
exit, unchanged auth bytes and released home ownership. A discriminating
component case omits the nominated API key, returning before any bridge account
RPC; the pin still makes two refresh requests during startup. The milestone's
one-refresh and post-wake refusal oracle remains unchanged pending an operator
decision. These component witnesses do not satisfy the retained M2 negatives.

### M2 canonical negative oracle amendment (2026-11-17)

The binding 15:39Z coordinator answer replaces the legacy post-wake negative
oracle with pre-admission rejection. At rust-v0.160.0 invalid synthetic auth
makes zero refresh requests; expired synthetic auth makes exactly two during
initialization. Constants name the pin so a changed count requires remeasurement.
Both cases require exactly one paired seq=1 relink Error, no Ready, Listening,
Connected, bridge.ready, run.started or live.closed, and zero Wyoming admission
through cancellation, calls, sidebands, media and external requests. The CLI
exits 1 with exactly `echo-bridge: authorization`, preserves auth bytes and
releases home ownership. All success requirements remain unchanged.

The scenario and full-frame oracle migrate together. The runner uses the
existing workspace echo-codex Supervisor only to provision/reacquire private
synthetic homes, adding no third-party package or credential path. The actual
bridge and verified app server perform authentication; a bound loopback listener
observes absence of device admission. Component executable tests protect the
same boundary. These negatives cannot receive a wake because authentication
precedes Wyoming readiness.

The executable media fixture now echoes a bounded five-packet reply per wake,
then consumes remaining microphone input without sending more reply packets.
It sends transcript completion only after the third paced Wyoming frame, a
causal observation that the two streaming converters accepted that reply.
Each wake explicitly rearms the remote. Exact packet counts, PCM energy,
played receipt, reuse and kill/idle assertions remain. This removes the fake's
unbounded post-completion audio source without relaxing production late-audio
rejection.

### Owner-scoped home unlock (2026-11-17)

The coordinator's 16:48Z answer and 16:58Z test-placement amendment authorize
explicit lease unlock in Supervisor Drop after its child is confirmed reaped
or absent. Shutdown alone still holds exclusive ownership until Drop. If Drop
cannot confirm reaping, it retains the existing kill-on-drop and file-close
behavior without explicit unlock. The private field is named `lease` now that
Drop uses it directly.

On Unix a forked copy can retain the same open file description before exec,
even with close-on-exec set. Closing only the owner's File does not release a
flock while such a copy survives. Explicit unlock releases the description's
lock. The causal observation was a closed owner followed by refused acquisition;
the transient holder was unidentified. A deterministic private Unix regression
retains a std File::try_clone across owner Drop, protects live-owner exclusion,
and requires immediate reacquisition without retries. Existing public lifecycle
tests protect clean shutdown, persistent auth bytes and independent sessions.
No dependency, unsafe code, public accessor or credential path is added.

### Delegated tool consumer (2026-11-17)

`echo-delegation::codex_dispatch` consumes the existing pinned tool envelopes
without opening a model client or writing a delegation append. It maps an
allowlisted MCP catalog to stable `tool_N` aliases in discovery order, and
retains the original name, description and schema. Unknown aliases, namespace
overrides, malformed arguments, foreign or duplicate calls cannot reach the
tool host. The host still owns MCP authorization and confirmation; discovery
and model intent grant neither.

One dispatcher belongs to one thread in one process generation. A matching
turn admission starts an absolute deadline; calls cannot renew it. Dispatch
reserves IDs before effects, limits attempts to the smaller of configuration
and eight, and never retries a cancelled or ambiguous effect. Budget and
deadline outcomes carry the matching turn interrupt for the IO owner to send.
The owner must poll dispatch alongside IO/media and schedule the deadline even
without a tool request. Completion only clears a matching active turn. A
separate daily counter must survive calls and admit before model work; it
resets on a forward UTC day and fails closed on backward clock movement.
Server error payloads become fixed failure text, while successful tool content
and MCP `is_error` retain their meaning in the pinned result envelope.

The adapter adds internal `echo-codex` and already-locked Tokio time dependencies
to `echo-delegation`; no third-party package or shared wire contract changes.
Component tests prove its memory-only lifecycle and real loopback MCP boundary.
Canonical voice-owner wiring, approval presentation, real-pin M4, exact goldens
and the dropped-result seed remain required before story 002 can pass. The
retained legacy runner remains available for its existing consumers until the
canonical owner is wired; these component results do not prove its replacement.

### Caller-driven dispatch scheduling (2026-11-17)

The bridge projects matching `turn/started` and `turn/completed` notifications
from the pinned JSONL envelope (`threadId`, `turn.id`). Malformed matching IDs
close the generation; foreign-thread lifecycle notifications do not admit work.
Its call owner queues a matching `turn/interrupt` and retains the admitted RPC
across cancelled polls. A bounded acknowledgement failure closes the generation.
Media polling continues while that acknowledgement is pending, and accepted
notifications remain in the existing bounded IO queue.

`ToolSession` borrows a connected media call and supplies the existing voice
port boundary. It retains one dispatch future across PCM and transcript polls,
serializes queued requests with a 32-event limit, and schedules silent-turn
expiry. Matching turn completion cancels pending work and retires the turn
before queued requests can execute. Results use their original server request
IDs. Dropping this adapter drops its work; the caller still owns and must await
call/media cleanup. There is no spawned worker, model client or second reader.

Memory-only tests protect delivery, interleaved PCM/transcripts, expired and
exhausted turns, cancelled polling, queued and duplicate requests, queue
overflow, safe failures and cleanup. These are component observations. Canonical
MCP/approval and daily-cap admission wiring, actual-pin M4, full goldens and the
dropped-result seed remain required for story 002. No shared protocol change or
new dependency is introduced by this scheduling adapter.

### Tool dispatch across conversation runs and idle (2026-11-17)

The conversation driver now consumes its existing `VoicePort` boundary during
both microphone runs and idle waits. `tool_conversation` retains a `ToolSession`
across successive wakes on one media call. Dispatch therefore continues while
the device is idle, and does not reset turn reservations with each reply.
Every exit drops pending dispatch before awaiting signaling/media cleanup;
failed construction and observers use the same cleanup path. The original
conversation entry point still uses the raw media call and its existing rules.

Memory-only component tests observe exact result envelopes during a run and
idle, one media negotiation for two wakes, unchanged microphone frames, played
settlement, cancellation without fabricated results, and bounded cleanup on
configuration or observation errors. Executable policy selection, real-pin M4,
full goldens and the dropped-result seed remain unproved. This introduces no
shared protocol change, external dependency or credential route.
