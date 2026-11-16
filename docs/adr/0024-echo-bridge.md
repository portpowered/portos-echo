---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: accepted
---

# ADR 0024: Bridge audio orchestration

`echo-bridge` owns the Wyoming to Live journey, using the existing `echo-audio`,
`echo-config`, `echo-contracts`, `echo-live` and `echo-wyoming` public APIs.
It reuses the locked `tokio`, `serde_json` and `thiserror` dependencies;
tests reuse `tempfile`. No external package is introduced.

Configured delegation mounts the existing `echo-delegation`, `echo-agent-loop`
and `echo-mcp-client` crates. The header adapter reuses locked `reqwest` types;
component tests reuse `echo-spoof-mcp`. These add dependency edges only, with no
new locked package. The first configured MCP server is the selected host, with
its existing exact tool allowlist. Bearer-file auth reads only its nominated,
bounded file. Unauthenticated spoof mode supplies an empty bearer because the
existing MCP client always sends that header; no credential is synthesized.
Port OS device-code onboarding is outside this slice and fails closed here.

One runner is retained between completed delegations. Only one delegation is
pending at a time; other targets or concurrent distinct requests are refused.
Duplicate ids are remembered within the Live call, with a bounded 1024-id set.
The worker owns no Live socket. The active call owner sends prepared commentary
and only then records `session.commentary.append`. A bounded sink adds run and
delegation correlation to tool dispatch attempts. It omits raw payloads and masks
unlisted tool names. Cancellation abandons the runner and its backend workers;
replacement sinks cannot attribute late observations to a new run. Host UTC-day
request reservations survive replacement and never refund uncertain dispatch.
Exhaustion returns fixed limit commentary without claiming an auth failure.

The application owner accepts an explicit UTC-day source and shutdown receiver.
The binary supplies real UTC and stdin EOF. Delegation reservations consult that
same host source after credential acquisition; cancellation never refunds them.
Monotonic call and journey deadlines use Tokio's clock, converted at the Live
boundary, so component T3 can advance application time without changing system
time. Shutdown abandons pending delegation before joining network cleanup.

`mic.frames` records the first successfully sent microphone batch. Exact final
packet/sample/drop totals are in `mic.completed`; `wyoming.mic_frames` stays
compatible. `reply.started` follows the first forwarded PCM payload, so arming
an empty reply no longer claims audible activity. A Played acknowledgment is
still required for terminal playback evidence.

One multi-thread Tokio runtime drives one Live call owner. Conversion uses
fixed scratch and complete 20 ms Opus frames. Reply buffering is bounded and
evicts oldest frames. The local health listener accepts one bounded request
at a time, with a deadline. Trace records contain fixed categories and counters.

Lab mode validates all three existing lab inputs before dialing and uses only
synthetic credentials. The echo adapter completes after ten microphone chunks
have been converted, all packets echoed and their count acknowledged. This
adapter does not establish real provider completion vocabulary. Unknown events
never imply success. The coordinator's M2 gate owns real provider compatibility.

Optional `ECHO_LAB_SESSION_IDLE_SECONDS` selects the idle interval only when all
three lab inputs are present. Its decimal `u64` grammar includes zero; absent
means 120 seconds. Invalid values and any override in configured mode fail
offline. This permits delivered-binary expiry proof within the T3 time budget
without changing configured TOML behavior, credentials or the lab stdout protocol.

Configured runs use `echo-signin` providers for Live credentials in both modes.
Operator commands dispatch before run initialization; import requires an explicit
source and leaves it unchanged. No home-directory credential discovery is used.

Configured runs host the existing `echo-screen-server` page and authenticated
stream on literal IPv4 loopback. The nominated pairing file is bounded to 16 KiB,
UTF-8, nonblank and free of control characters after removing one LF or CRLF
terminator. Token or bind failure prevents readiness and Wyoming connection.
The token never appears in stdout or trace; `screen.listening` records only the
bound address. The synthetic lab route does not require a screen or pairing file.

The bridge publishes Ready initially, Listening on wake, Speaking after the first
forwarded PCM payload, and Ready after the acknowledged run completes. A bounded
32-state channel feeds the screen library; capacity or closed-input failure is
fatal rather than silently losing activity updates. EOF shuts down the screen
with the other owned resources. Relink and delegated activity remain part of the
same bridge assembly, using the existing screen contract.

Configured call-creation and model-credential acquisition failures keep both listeners alive, set
`live=error` and `relink_required=true`, and emit `auth.relink_required` with a
fixed reason. Later wakes emit `run.refused` without another credential or Live
request. Explicit sign-in followed by restart clears this in-process latch; a
file change alone does not. The paired screen displays the operator sign-in and
restart instruction. Its Relink action repeats that instruction without starting
authentication. Call-creation outages display a non-relink error and permit the
next wake to try again. The synthetic lab route retains its fatal-error protocol.
Active producer failures cut playback while its receiver remains alive and wait
for the satellite's Played acknowledgment before publishing the safe error.
They close the failed call and abandon delegation without recording successful
commentary or playback. Ordinary model outages retain fixed failure commentary;
request-cap refusal remains distinct from relink.
The health schema also reserves the `kill_switch` boolean, initially false;
durable kill control remains the next retained story.

The optional `[audio]` table sets `reply_lead_ms`, default 200, as a positive
multiple of 20 that fits `u16`. Existing configurations keep the default. The
bridge sends at most ten initial frames at that default, then uses echo-audio's
20 ms Pacer without catch-up bursts. Underflow does not replenish initial credit.

The provisional `response.done` event with `response.status = "cancelled"`
stops production. The bridge clears its complete backlog and calls Wyoming's
additive `cut_with_stats`. Wyoming joins its source pump before counting retained
source and queue frames; prior capacity evictions are excluded. Only the session
writes the early stop, and playback still requires real `played`. `reply.cut`
records `bridge_dropped`, `transport_dropped` and their sum `dropped_frames`.
Neither partial resampler tails nor audio already transmitted are counted.
The same record reports `bridge_evicted` separately for earlier bridge capacity
evictions and `reply_frames` for complete decoded frames offered to its queue.
With no transport eviction, transmitted plus retained cut plus earlier bridge
evictions equals `reply_frames`; the cut never counts an earlier eviction again.
`reply.overflow` reports the first bridge eviction per run with capacity 128.
The overload witness sends bounded synthetic batches until this observation;
it never assumes that a UDP burst arrives without loss.

The pinned-host timing regression reuses one bridge and host for ten trials.
The host's FIFO observer timestamps cancellation dispatch at playback start plus
one second, before instructing the fake to send the event. Sink EOF minus that
timestamp includes controller/pipe dispatch overhead. It never substitutes
`played` for sink closure or treats a missing EOF as zero. The fake offers 150
encoded 20 ms frames per trial. No physical silence or provider claim follows.

Healthy completed runs retain their Live call for `live.session_idle_seconds`.
The call owner drives WebRTC while idle and discards idle audio. Each wake gets
fresh codecs and counters, while RTP sequence/timestamps and the synthetic
fake's cumulative packet acknowledgment remain scoped to the retained call.
Idle expiry wins at the exact deadline; an unrepresentable `u64` duration has
no reachable expiry rather than panicking or narrowing the config contract.
Cancellation closes this provisional call before accepting another wake, so
late audio cannot cross the cut into another run.

Idle describe probes observe the existing Wyoming reconnect supervisor; the
bridge never creates a competing dial loop. Health and fixed trace events show
loss and recovery. Registration remains the supervisor's responsibility. EOF
cancels probes and closes the owned call, client and health listener.

The remaining lifecycle failure matrix and final quality proof stay
in this lane. Until final assembly and quality proof are complete the lane is
not ready for acceptance.
