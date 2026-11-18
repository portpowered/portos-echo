---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 17
status: amended; str0m choice superseded, explicit encoded transport retained
---

# 0009: Explicitly driven WebRTC transport

## Retirement amendment (2026-11-17)

The binding el-w2d-retire decision replaces the private str0m backend with
`rtc =0.21.1`, the Sans-I/O core already locked for `echo-media`'s
`webrtc =0.21.1`. Only the direct rtc edge is needed: the async webrtc driver
would introduce workers into this caller-driven API. No package version moves
or new package identities are authorized. The original decision below remains
historical; its str0m-specific buffer and implementation details are superseded.

The public encoded Opus, event, call and fake APIs remain active. One owner
still drives UDP, protocol deadlines and cancellation; no workers are spawned.
Readiness still requires connected ICE and the opened `oai-events` channel.
Audio sequence numbers, timestamps and encoded payloads pass through unchanged.
The generated SCTP m-line's redundant sendrecv attribute is omitted from the
signaled SDP, preserving one sendrecv audio track and no video. Receive queues
retain 128 audio packets (drop oldest) and 16 texts (reject overflow). Text
admission remains 16 KiB with a 128 KiB outstanding-send bound. Errors retain
their sanitized public categories. Close/drop release the socket immediately.
Signaling starts one microsecond before the initial injected tick to accommodate
rtc's strict positive elapsed-time check for host nomination, including Windows
clock resolution. Subsequent input, media and timeout times remain exactly the
caller's; a frozen bridge clock can negotiate with an advancing remote peer.
The adapter also drains event-triggered DTLS/SCTP work to quiescence, servicing
the initial DTLS flight at the last supplied instant. Sequential calls can
negotiate and deliver events with both clocks frozen, without a timer advance.

The canonical bridge path uses Codex and echo-media. Legacy/Muse bridge calls,
M3, and simulator fake peers still consume echo-live. Sign-in and Responses
consumers still require echo-chatgpt-auth and echo-agent-loop. Their registrations,
coverage floors, mutation gates and layering restrictions therefore stay active.
Retirement does not authorize caller migrations, runtime selection changes or
shared contract changes. Rollback reverts this replacement and its lockfile;
no persisted state changes. Codex LICENSE/NOTICE and design pin remain intact.

## Original decision (superseded transport selection)

Use pinned str0m 0.25.0 with default features disabled and `rust-crypto`,
and a Tokio UDP adapter in `echo-live`. One owner drives each peer with
explicit protocol time. There are no background tasks or process-global
crypto initialization. RTP mode preserves encoded Opus bytes, sequence numbers
and timestamps. The crate performs no encoding, decoding or provider signaling.
It carries opaque event strings; provisional `echo-contracts::live` types stay
with the later provider consumer rather than becoming a second event vocabulary.

## Candidates

The released package source, manifests and upstream documentation are the
comparison inputs. Versions below are trial pins, not floating recommendations.
Maintenance means a published release and maintained upstream project; it is
not a claim about a support SLA. Raw commands and results belong in the PR comment.

| Candidate | Maintenance and license | Execution, audio and data | Windows and Linux; trial disposition |
| --- | --- | --- | --- |
| [str0m 0.25.0](https://docs.rs/str0m/0.25.0/str0m/) | Published release; [upstream](https://github.com/algesten/str0m); MIT OR Apache-2.0 | Sans-I/O; encoded RTP/Opus and SCTP data channels; adapter owns UDP and time | Windows MSVC compile and delivered two-peer tests; Linux GNU through WSL with the same tests. Selected for explicit time, ownership and absence of hidden workers. |
| [webrtc 0.21.1](https://docs.rs/webrtc/0.21.1/webrtc/) | Published release; [upstream](https://github.com/webrtc-rs/webrtc); MIT OR Apache-2.0 | Async wrapper over rtc Sans-I/O core; default Tokio driver starts an internal worker; RTP tracks and data channels | Windows compile preflight and unchanged license/advisory check succeed. Linux build supported upstream, not measured for this candidate. No candidate loopback claimed. Viable alternative, but its async driver adds lifecycle machinery the explicit S1 clock does not need. |
| [datachannel 0.16.1](https://docs.rs/datachannel/0.16.1/datachannel/) | Published Rust wrapper; [upstream](https://github.com/lerouxrgd/datachannel-rs); MPL-2.0 | Native libdatachannel C++ runtime through FFI; media feature for RTP tracks, data channels | Windows/Linux require native build/link tooling. License preflight rejects datachannel and datachannel-sys under unchanged deny.toml. Neither platform compiled or run; no license exception or FFI lane added. |

`rust-crypto` does not mean the entire selected closure is pure Rust:
str0m's dimpl/rcgen closure also uses aws-lc-rs and aws-lc-sys. A working
native C/C++ build toolchain is required on Windows and Linux. Default str0m
example/web-server dependencies are disabled. Keep Cargo.lock, version and
features together when reproducing the result. Review owns final hosted checks.

## Lifecycle and bounds

`LivePeer::bind(address, now)` advertises only that explicit bound host
candidate. Callers choose the address; S1 uses only 127.0.0.1 with ephemeral
ports, with no STUN/TURN, mDNS or interface enumeration. SDP crosses the caller's
signaling boundary through `offer`, `answer` and `apply_answer`. All mutate
and drain the core through its next Timeout. No HTTP or credential handling exists.

After signaling, call `flush` for the next protocol deadline. Await `readable`
then call `receive(now)`, or call `timeout(now)` when the deadline arrives.
The application must keep driving both peers, use monotonic injected time,
and impose its own operation deadline/cancellation. `ready` requires connected
ICE and an opened `oai-events` channel, rather than SDP acceptance alone.
The test driver uses paused per-test Tokio time and readiness, never fixed sleeps.

Outbound audio sends one packet and drains the core; there is no application
outbound queue. The core's audio send buffer is bounded (50 packets by default).
Received audio retains 128 packets and drops the oldest with an observable
counter. Received text retains 16 messages; overflow reports `Full`, preserving
previously accepted FIFO messages. It cannot silently claim delivery of the
overflowing message. Drain receives promptly; treat receive overflow as a failed
journey and close. SCTP's send buffer is bounded (128 KiB by default), and send
pressure returns `Full` before accepting text. Messages are limited to 16 KiB;
audio payloads are limited to 1200 bytes. Already accepted sends are never
automatically replayed. Callers own retry policy for rejected sends.

Errors omit raw SDP, wire payloads and library diagnostics. `close` is
idempotent, releases the socket and retains received messages for draining.
After close, sends and readiness/receive operations return `Closed`. Cancelling
readiness is safe; cancelling a send/flush can leave an accepted packet's
delivery uncertain, so close that journey rather than retrying blindly.
Dropping the peer also releases its socket; there are no owned worker tasks.

## Reproduction and limits

From the worktree, with Rust 1.100.0 and the native build tools installed:

~~~sh
cargo test --locked -p echo-live --lib
cargo nextest run --locked -p echo-live --all-targets --config-file .github/nextest.toml \
  --profile ci -E 'test(t3_)' --success-output immediate
make test-unit
make ci
~~~

The T3 witness asserts exactly 100 packets in each direction, sequence 0..99,
960 timestamp steps at 48 kHz and exact inline 20-ms Opus comfort-noise bytes.
It asserts three intact JSON strings per direction, exact bytes, IDs and order.
The negative cases cover unanswered negotiation/deadline, cancellation, close
and resource release; the concurrent journey uses distinct timestamps and IDs
to detect pair cross-talk. T1 checks malformed SDP/state and audio/text pressure.
This is a transport spike, not a decoded sound or assembled bridge demonstration.

W1's assembled hassmic-host journey remains gate L-W1. At L-M2, separately
authorized real GPT-Live calls must prove provider SDP compatibility and capture
whether `session.delegation.created` and `session.commentary.append` travel on
`oai-events`, the sideband WebSocket, or both. Local opaque JSON is not that
evidence. No real provider call, credential, household audio or milestone
completion is authorized or claimed here. R-CI, R-LOOP and R-MERGE remain review
gates. Rollback removes this crate, registrations and ADR, then regenerates the
lockfile; no persisted state or shared-contract migration is involved.

## Packages covered

The following names cover this crate and the exact newly introduced lockfile
package names, including target-specific and inactive optional closure entries.
Existing direct packages are included to make their use explicit.

- `aead`
- `aes`
- `aes-gcm`
- `arrayvec`
- `asn1-rs`
- `asn1-rs-derive`
- `asn1-rs-impl`
- `aws-lc-rs`
- `aws-lc-sys`
- `base16ct`
- `base64ct`
- `block-buffer`
- `bytes`
- `cc`
- `ccm`
- `chacha20poly1305`
- `cipher`
- `cmake`
- `cmov`
- `combine`
- `const-oid`
- `cpubits`
- `crc`
- `crc-catalog`
- `crypto-bigint`
- `crypto-common`
- `ctr`
- `ctutils`
- `curve25519-dalek`
- `curve25519-dalek-derive`
- `data-encoding`
- `der`
- `der-parser`
- `der_derive`
- `deranged`
- `digest`
- `dimpl`
- `dunce`
- `ecdsa`
- `echo-live`
- `elliptic-curve`
- `fastrand`
- `ff`
- `fiat-crypto`
- `find-msvc-tools`
- `flagset`
- `fs_extra`
- `ghash`
- `group`
- `hkdf`
- `hmac`
- `hybrid-array`
- `inout`
- `is`
- `jobserver`
- `log`
- `minimal-lexical`
- `mio`
- `nom`
- `num-conv`
- `oid-registry`
- `p256`
- `p384`
- `pem-rfc7468`
- `pkcs8`
- `pkg-config`
- `poly1305`
- `polyval`
- `powerfmt`
- `ppv-lite86`
- `primefield`
- `primeorder`
- `rand_chacha`
- `rcgen`
- `rfc6979`
- `rustc-hash`
- `rustc_version`
- `rusticata-macros`
- `rustls-pki-types`
- `sctp-proto`
- `sec1`
- `semver`
- `serdect`
- `sha1`
- `sha2`
- `shlex`
- `signature`
- `socket2`
- `spki`
- `str0m`
- `str0m-proto`
- `str0m-rust-crypto`
- `subtle`
- `thiserror`
- `time`
- `time-core`
- `time-macros`
- `tokio`
- `tokio-macros`
- `tracing`
- `tracing-attributes`
- `tracing-core`
- `typenum`
- `universal-hash`
- `untrusted`
- `wasi`
- `wnaf`
- `x25519-dalek`
- `x509-cert`
- `x509-parser`
- `yasna`
- `zeroize`
