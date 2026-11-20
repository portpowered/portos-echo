---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: active; measured facts, each with its date and source
---

# Measured baselines

Windows measurements and installation procedures below are historical evidence only.
Since the owner decision of 2026-11-18, Linux is the only supported build, test and runtime platform.
Measurement values, dates and their original limitations are retained.

Facts measured on the real device, bench and services. Each row has its date and how it was measured. Plans and
factory payloads cite these instead of rediscovering them. Add a row whenever a measurement changes a decision.

## Development profile size (2026-11-17)

The owner requested smaller dev/test output. These initial measurements use source `707282e`, Rust 1.100.0
(`0ef1bc386`), native Windows MSVC and isolated clean targets. Baseline profiles were Cargo defaults; the
candidate sets workspace dev debuginfo to `line-tables-only` and dependency debuginfo to `false`, with test
inheriting dev and incremental unchanged. These are clean-build measurements, distinct from the supplied
41 GB stale-target observation. The Windows observations are historical; Linux measurements below establish
the required size reduction, while coverage parity remains unproved.

| Measurement | Default profiles | Candidate profiles |
| --- | --- | --- |
| `cargo build --locked --workspace --all-targets` | Exit 0; 121.735 s | Exit 0; 100.687 s |
| Allocated target after build (`du -sh`, `du -sk`) | Separate checkpoint unavailable: initial size command timed out | 3.4G; 3,501,686 KiB |
| Target after nextest compilation | 11G; 10,807,462 KiB; exit 0 | 3.4G; 3,518,830 KiB; exit 0; 26.344 s |

The baseline nextest command was `cargo nextest run --locked --workspace --all-targets --no-run
--config-file .github/nextest.toml --profile ci`; its total wall time was not captured. The original long-path
Windows checkout failed with MSBuild FTK1011; the shorter-path recovery above compiled successfully.
An external dependency-free scratch test built with the candidate profiles and run with `RUST_BACKTRACE=1`
exited 101 as intended, with an actual stack frame at `src/diagnostics.rs:2`. Allocated Windows output after
the complete build/nextest sequence fell 67.4%. Existing `echo-config` `config_load` tests passed (exit 0;
7.266 s) with the candidate profiles. These timings are single observations on a shared host.

Coordinator-run Linux measurements on source `c2a1a63`, relayed 2026-11-17 at 09:35 UTC, used isolated clean
baseline/candidate targets and the same source with only the two dev profile tables removed in the baseline.
The shared pe-ci lock was acquired at 09:18:03 UTC. Rust was 1.100.0 (`0ef1bc386`), LLVM 23.1.5,
`x86_64-unknown-linux-gnu`, nextest 0.9.152. Commands match the Windows build/nextest sequence above.

| Linux measurement | Default profiles | Candidate profiles |
| --- | --- | --- |
| All-target build | Exit 0; 54.057 s | Exit 0; 48.728 s |
| Allocated target after build | 9.2G; 9,543,200 KiB | 3.4G; 3,469,964 KiB |
| Nextest compilation | Exit 0; 0.604 s | Exit 0; 0.678 s |
| Allocated target after nextest | 9.2G; 9,573,868 KiB | 3.4G; 3,500,596 KiB |

Linux output after build/nextest fell **63.44%**. Timings are single observations on shared capacity, not
portable latency guarantees. Candidate `config_load` passed (exit 0; 2.809 s). The external compiled scratch
panic exited 101 with actual backtrace frames at `./src/diagnostics.rs:2:26` and `:2:23`.

Coverage used `make coverage COVERAGE_BASE=b54b627`, nightly-2026-11-05, LLVM 23.1.5 and cargo-llvm-cov 0.9.1.
Candidate coverage exited 0 (269.995 s), passing every floor: workspace 52,481/53,697 lines (97.73%),
echo-bridge 12,130/12,354 (98.18%), echo-lab 15,528/16,116 (96.35%); all 21 scope counts are in the PR comment.
Baseline coverage exited 2 (177.624 s): the existing idle-probe failure/recovery test failed at
`mcp_failures.rs:161:13` (95 passed, 1 failed). Thus no per-crate comparison or coverage parity is claimed.
The coordinator identified the main-branch fix in PR 152 and directed a rebase followed by a coverage-only
same-source pair; no size or diagnostic rerun is required under that binding response.

The requested coverage-only pair ran on source `732ed61`, with both variants using
`make coverage COVERAGE_BASE=407ccef054a57071a87e5b97bfcece96cc9a5dd0`, isolated targets and the
shared lock (acquired 09:50:47 UTC). The coordinator relayed results at 10:03 UTC. Candidate coverage exited 0
(376.057 s): workspace 59,812/61,261 lines (97.63%); every unchanged line, branch and changed-line floor passed.
All 23 scope counts are published in PR 157. Baseline exited 2 (306.977 s):
`bridge_refusal_live_error_before_pcm_then_same_connection_recovers` failed at
`crates/echo-bridge/tests/bridge_refusal/mod.rs:82`, reporting 1359.430 ms against a 1000 ms ceiling.
No baseline LCOV was produced. Per-crate comparison: **not obtained: baseline run hit the known
refusal-timing flake fixed by #155**. No measured parity or per-crate delta is claimed.
The coordinator explicitly accepted A-3/E-COV on the passing candidate floors plus this baseline-failure
explanation, and directed no further coverage pair. This supersedes the pending comparison disposition
above; historical observations remain scoped to their measured source. Hosted CI and independent review
own final-head quality and validation.

The coordinator's 2026-11-17 amendment leaves one-shot incremental suppression inconclusive: ci-latency is
unproved, the full local CI comparison is withdrawn, the optional coverage pair was not run, and default
incremental is retained. Hosted CI is the quality gate. Raw output and per-crate evidence belong in PR 157
comments; no scratch or evidence files are committed.

## The device

| Fact | Value | Measured |
| --- | --- | --- |
| Model | Echo Show 8 (2nd gen), `ro.product.model` `AEOAT`, device `athena` | 2026-11-15, `adb shell getprop` |
| OS | Fire OS 7.0 (Android 9), build `PS75751.7809N` | 2026-11-15, `getprop ro.build.display.id` |
| ABI | `armeabi-v7a`, 32-bit userland | 2026-11-15, `getprop ro.product.cpu.abi` |
| Memory | about 1.9 GB (`MemTotal` 1973744 kB) | 2026-11-15, `/proc/meminfo` |
| Access | rooted; `adb shell` runs as `uid=0` | 2026-11-15, `adb shell id` |
| Assistant software | `hassmic` 2026.11.13.185908 (echo-dot-assist `8646a4f`, the signed release build) installed in `/system`, running as `hassmic -P wyoming -D -E`: a Wyoming satellite on TCP 16700, the microphone streaming during replies, no wake sound. Its self test passes on 7809. Alexa's apps are off and out of `/system` (kept in `/system/hassmic-removed`), and hassmic's lockdown limits Amazon's programs to the LAN | 2026-11-15, `boot.log` (`wyoming on 16700, wake=local`, `self test: passed`), `netstat` (LISTEN `0.0.0.0:16700`), `/proc/<pid>/cmdline` |
| Wyoming from the operator's PC | `describe` over the LAN answers `info` with the satellite's name | 2026-11-15, a TCP client on the PC |
| Wake word on the Echo | "Computer": the firmware's complete set `/system/local/models/keyword/en-US/COMPUTER/` copied unmodified to hassmic's model set `computer-en-US` (`wake word: computer-en-US "Computer" (en), active`). Offline, the firmware's `/system/hassmic/pryon_test -m <set>/pryon.manifest` accepted "COMPUTER" in 30 of 30 VibeVoice clips of "Computer, what time is it?" (16 kHz mono, onset 0.58 s to 1.14 s on the first) and in 0 of 30 clips without the word. Over the air (T5, 2026-11-16 03:18): three of those clips, padded and resampled to 48 kHz and played through the Ring camera's speaker, each produced a `wake: COMPUTER` line in hassmic's `boot.log` (5 lines for 3 clips; the first line of a burst is `type=0`, repeats are `type=2`). Nothing follows the wake while no Wyoming client is connected to port 16700, so the Echo shows no listening state and gives no answer until `echo-bridge` connects | 2026-11-16, `boot.log`, `pryon_test`, Ring `play-audio` |
| A wake reaches a Wyoming client (M1, Echo side) | A minimal client on the operator's PC connected to 16700 and sent `describe` then `run-satellite`. A Ring-played "Computer, what time is it?" then produced, 13–14 s in, one `run-pipeline` (`start_stage` `asr`, `end_stage` `tts`) and a stream of `audio-chunk` (16 kHz, 16-bit, mono; about 1,530 chunks, about 32 s) until hassmic's `pipeline timeout` returned it from listening to idle. Three of three runs did this. The stream opens with 800 ms of zeros on purpose (`LEAD_IN_MS`, hassmic `main.c`, for Home Assistant's VAD), then replays from the wake word's tail. Through the Ring's speaker that speech arrives at about −26 dBFS for about 0.25 s over a −58 dBFS floor; Whisper small.en transcribed it as "Hold on." and, for the phrase played twice, "Computer louder." hassmic reports `wake_word_name: "alexa"` and `active_wake_words: ["alexa"]` whatever word is loaded (hard-coded in `proto_wyoming.c`) | 2026-11-16, hand client, `boot.log` |
| Screen client on the Echo | A minimal WebView app as hassmic's home app shows the screen server's page in 1280×774 (hassmic keeps a 16 px grip at the top and its 10 px light band at the bottom); WebView `com.amazon.webview.chromium` 138; the app's PSS 70 MB, its renderer 164 MB RSS; Used RAM 630 MB with hassmic's face, 852 MB with the page; focused 33 s after `adb reboot` | 2026-11-15, spike S3 (`docs/spikes/s3-screen-client.md`) |
| Wake word models | Pryon models for "Alexa", "Amazon", "Computer" and "Echo" in `/system/local/models/keyword/<locale>/` (21 locales); hassmic supports all four. "Computer" is the decided wake word (2026-11-16); its en-US set is complete, `pryon.manifest` included | 2026-11-15, `ls /system/local/models/keyword/en-US/` |
| Free memory with stock Alexa running | `MemAvailable` about 670 MB | 2026-11-15, `/proc/meminfo` |
| SELinux | `Enforcing` (the unlock's policy; hassmic's install extends it) | 2026-11-15, `getenforce` |
| Installing from Windows | echo-dot-assist's scripts run from Git Bash with `MSYS_NO_PATHCONV=1` (otherwise adb gets `C:/Program Files/Git/data/...`). `scripts/otatool.py` needs `os.fchmod`, which Windows Python has only from 3.13. With 3.11 or 3.12, a `sitecustomize.py` that defines it as a no-op (on Windows it only sets read-only) lets the bundle verify and unpack; the Echo-side `chmod` sets the real modes. `HASSMIC_FIRMWARE_OK=1` answers the 7809-vs-7080 question | 2026-11-15, the install run |

## Firmware probe of build 7809

Probed 2026-11-15, read only: the same reads as echo-dot-assist's `scripts/device/probe.sh`, run over `adb shell`
without pushing a file, plus `getevent -lp`, `getevent -il`, `/proc/bus/input/devices`, `tinymix`, `dumpsys input`,
`/proc/asound` and `wm size`. Raw output stays in the operator's artifact directory; it holds the device serial.

echo-dot-assist's `devices/athena` profile is pinned to 7080 (`probe.md5`), verified on 7077 and 7082, and was
installed and self-tested on 7806 (2026-11-01), whose `libpryon.so` and `libaudioflinger.so` differ from 7080's.

| File (under `/system`) | 7809 md5 | Same as 7080 |
| --- | --- | --- |
| `lib/libpryon.so` | `5f213d89d39114c6b5d58fd6489b5598` | No |
| `vendor/lib/libasp.so` | `ac5e8985aa9d67461d3b054bde179363` | Yes |
| `lib/libaspclient.so` | `aeb4f3dc3d9ace3157d5c36bf9739836` | Yes |
| `lib/libaudioflinger.so` | `9318b38640edfdba31c3663e202df0d4` | No |
| `vendor/lib/hw/audio.primary.mt8183.so` | `cc54a24e1499090ac648adccae2033dd` | Yes |
| `lib/libwakewordserver_jni.so` | `0926fadcaefc0b8723a6d7d5988cae54` | Yes |

The two that differ are the two that differed on 7806. echo-dot-assist does not record 7806's hashes, so whether
7809's are byte-identical to 7806's is unknown; hassmic's setup will ask before installing (`HASSMIC_FIRMWARE_OK`).
Kernel `4.4.146+`, SDK 28, incremental `0036811211140`.

Re-probed 2026-11-15 19:32 UTC with `echo-lab probe` (PR 38) by the coordinator under T5: the build, kernel, SDK, all six
hashes, the display and the amplifier value above reproduce exactly, and 21 keyword-model directories are listed.
The device serial appears in none of the 41 saved artifacts.

| Signal | 7809 value | Matches the athena profile |
| --- | --- | --- |
| Volume keys | `gpio-keys`: `KEY_VOLUMEDOWN`, `KEY_VOLUMEUP` | Yes |
| Mute latch | `gpio-privacy-state`: `SW_MUTE_DEVICE`; `/sys/devices/platform/gpio-privacy/state` read `0` (unmuted) | Yes |
| Mute button | `gpio-privacy-button`: `KEY_POWER` | Yes |
| Touch | two devices: `fts_ts` (`ABS_MT_POSITION_X` 0–1280, `Y` 0–800) and `amazon_touch` (0–1279, 0–799). Which one carries the glass is not yet known (plan section 8.6, layer 4) | New fact |
| Light sensor | `tcs3400_als` | Yes |
| Display | 1280 × 800, density 213 | Yes |
| Speaker amplifier | card 0 `mt-snd-card`; control 83 in tinymix's list `Master TAS5805m Playback Volume` = 100 | Yes |
| Event node numbers | `event0`–`event5` this boot; never a stable id, devices are found by name | — |

## The test bench

The bench is a Ring pan-tilt indoor camera next to the Echo, driven by the Port OS CLI (`port`, v0.1.27 or later)
under one held camera session.

| Fact | Value | Measured |
| --- | --- | --- |
| Held session listens and talks | `port session hold <ring> --camera --listen` reports `listening: true`, `talking: true` | 2026-11-13 |
| Ambient room level | about −70 dBFS mean over 3 s (`port message record --audio-only`) | 2026-11-13 |
| Speaker to microphone delay | 320 ms from send to first heard, for the full 0.5 s chirp (`play-audio --chirp --listen 3`) | 2026-11-13 |
| Stock Echo answers a question played through the Ring | "Alexa, what time is it?" spoken by the `you` factory's local `tts` model, normalized to −1 dBTP, answered; Whisper `large-v3-turbo` transcribed "The time is 7:49 p.m." | 2026-11-13 |
| Ring microphone during its own playback | sometimes ducked to about −80 dBFS while the Ring speaker plays; record replies after the clip ends | 2026-11-13 |
| Synthetic voices | quiet or robotic Windows voices woke the Echo but were rejected as false wakes; a natural TTS voice at full level worked | 2026-11-13 |
| Held session lifetime | a hold can be preempted when another viewer takes the camera (`reason: preempted`); open a new hold and retry | 2026-11-13 |
| Wake word | the owner set this Echo's wake word to "computer" (2026-11-15); bench clips say "Computer, what time is it?" | 2026-11-15 |
| A play without `--listen` returns early | `play-audio --file` alone returns when the clip is queued, about 0.7 s after it starts and about 2 s before it ends; wait for the returned `playEndedAt` before the next command on the hold | 2026-11-15 |
| Snapshot during playback, before the fix | port main 5ef4727: a snapshot about 1.5 s into the 2.8 s question → the Echo answered with the time 13 of 22 times (3 no answer, 6 wrong reply); controls with no snapshot 15 of 15. Late talkback packets (worst 34 ms) and a keyframe request (never sent) ruled out; the Ring-side mechanism is unknown | 2026-11-15 |
| Snapshot during playback, with the fix | port main 179c4ab (portos-monolith PR 2741): a snapshot sent mid-clip waits until the clip stops, returns 2.3–3.4 s after it was sent, frame 0.38–0.49 s after `playEndedAt`; the Echo answered with the time 8 of 8, controls 2 of 2 (Fisher one-sided against before, p ≈ 0.035). Whisper `large-v3-turbo` on 8 s after the clip | 2026-11-15 |
| Ring talkback loss | Bare go-ring harness: 4/5 and 8/9 confirmed wakes; Port CLI: 4/6, 11/20 and 4/4 in separate windows. Coordinator matching playback to `boot.log`, 07:54–08:02 UTC: 12/16 full `wake: COMPUTER type=2` detections across CLI relay/direct and harness none/premic. Misses had no detection at all. Ring's media server acknowledged every packet over RTCP even for lost plays: successful play-audio delivery does not prove speaker playback. Hit onset was 1.3–1.8 s after clip start, with late hits at +3.6–3.8 s. These independent windows are not a pooled reliability estimate; physical replay recovery and wakes beyond six seconds remain unmeasured | 2026-11-19, coordinator playback/`boot.log` matching, supplied Ring replay mission |

Device-cycle acceptance allows at most one replay for a suspected lost say clip, with the first-try wake rate
and actual replay count reported separately. A positive wake never authorizes a loss replay; the existing
reply and recovery oracles still apply. M1 retains its 10 bridge and 10 hassmic-host restart recovery counts.
This policy and its synthetic tests do not establish physical replay reliability or a new T5 milestone.

Not measured yet: the Echo's listening bar in a Ring frame (the bar runs along the bottom edge of the screen,
which the current camera angle cuts off; see the B0 row under Milestones), and the Ring's lockout threshold for
repeated sessions.

## Services

| Fact | Value | Source |
| --- | --- | --- |
| Port OS MCP transport | stateless streamable HTTP: POST only, no `Mcp-Session-Id`, `GET /mcp` answers 405, no elicitation | Port OS plan review, 2026-11-13 |
| Port OS MCP auth | `/mcp` admits any access token with `token_use=access`, the issuer, the MCP audience, a subject and an active `grant_id`, whatever grant minted it. Authorization code + PKCE (DCR or CIMD) works on the pilot today; device-code tokens work once portos-monolith PR 2731 (merged 2026-11-15) is deployed | Port OS plan review, 2026-11-13; portos-backend coordinator, 2026-11-15 |
| Port OS device-code grant | `POST /auth/device-authorization` needs `resource` = the protected resource's `/mcp` URL (anything else: 400 `invalid_target`; none: a first-party token `/mcp` refuses with 401). Access token 24 h, refresh token 30 days bound by `cnf.jkt`; refresh tokens rotate and a replay revokes the grant. The built-in public CLI client already allows `device_code` and `refresh_token`; DCR and CIMD clients cannot use the device flow. Code lifetime 1800 s, poll interval 5 s | portos-backend coordinator, 2026-11-15 (PR 2731); `docs/user-guides/oauth/oauth-device-auth.md` |
| Port OS pilot device grant, live | 2026-11-15, from a fresh client: `POST /auth/device-authorization` (built-in CLI client, `endpoint:read`, `resource` = `https://api.portoperatingsystem.com/mcp`, ES256 DPoP proof), approved in a browser, then the `device_code` grant returns `token_type` Bearer, `endpoint:read` and a refresh token. `/mcp` `initialize` 200 and `tools/list` 200 with 10 tools (`control`, `endpoint_*`, `group_*`, `subscription_*`) on plain Bearer with no proof; `refresh_token` with the same key 200. Discovery on the auth and api origins advertises `device_authorization_endpoint` `https://auth.portoperatingsystem.com/auth/device-authorization`. PB-4 is deployed (portos-monolith PR 2735): a proof whose `htu` names `https://auth.portoperatingsystem.com/auth/token`, sent to `/auth/device-authorization`, now answers 400 (200 before PB-4), and the whole flow above still passes with a proof signed for each exact endpoint URL. Query strings in `htu` are ignored and the `Host` header is never trusted | portos-backend coordinator; first run on the pilot image from main `8b7b405`, PB-4 rerun on main `35aa60d` |
| Port OS pilot access token lifetime | 300 s, so a 30-minute run refreshes about six times | Port OS pilot runbook |
| Muse gadget SDK: sending | `musegadget send-user-msg [--session-id <uuid>] [--wait <s>] "<text>"`. A new `--session-id` starts a side chat and reusing it keeps later messages there; with it, the answer comes only from that side chat. `--wait` is 1 to 600 s and prints the answer. Exit 3: no answer in time, or Muse was still answering (what arrived is printed). Exit 78: the trust store is unreadable. Other exit codes and the stdout format are not documented. One waiting request per chat at a time. No credentials of its own: it talks to the local service over `/run/musegadget/musegadget.sock` | upstream `facebookincubator/muse-gadget-sdk` `linux/README.md` and `linux/AGENTS.md` (Apache-2.0), read 2026-11-15 |
| Muse gadget SDK: commands | One JSON file per command in `/etc/musegadget/commands.d/<name>.json` (`MUSEGADGET_COMMANDS_DIR` overrides). Names are lowercase words joined by dots; `system.`, `file.`, `device.` and `link.` are reserved. Fields: `description`, `exec` (argv, no shell), `required` and `optional` (each parameter `{type, description}`), `timeout_ms` (default 30000). The handler gets the parameters as one JSON object on stdin; a JSON object on stdout goes back to Muse; failure is a non-zero exit with the last stderr line as the reason; stdout and stderr are kept up to 96 KiB each. At most four commands run at once. New specs load only on `systemctl restart musegadget` | the same, read 2026-11-15 |

## The hassmic wire

| Fact | Value | Source |
| --- | --- | --- |
| Microphone stream from hassmic at pin `8646a4f` | The host sends `run-pipeline`, then `audio-chunk` events with positive payloads. It never sends a microphone `audio-start`: `start()` in `src/hassmic/proto_wyoming.c` emits `run-pipeline` and `audio()` emits `audio-chunk`. The reply direction is different, because there the client sends `audio-start` (`tests/fixtures/wyoming/duplex-reply.jsonl`). `echo-contracts` and S2 must accept a microphone stream with no `audio-start` | 2026-11-15, el-w0-hassmic-host captures `tests/fixtures/wyoming/wake-word.jsonl` and `duplex-reply.jsonl`, PR 39 |

## Reply cancellation (S2)

Measured by spike S2 (PR 44, `docs/spikes/s2-barge-in.md`) against `hassmic-host` at pin `8646a4f` with `-D`. The
reply was 3 s of 24 kHz s16 mono, sent whole up front and cut at 1 s, with ten trials per strategy. **No Wyoming action
discards audio hassmic has already received.** An early `audio-stop` drains the queue: the sink closes after a median
of 2004.3 ms (max 2005.4 ms), which matches the 2004.1 ms of doing nothing. `pause-satellite` and closing the
connection never closed the sink within 4 s. A wake (SIGUSR1) while speaking emits no new `run-pipeline`: under
`-D` the existing microphone stream continues. So the time to cut a reply is bounded by the reply audio the bridge
has already sent and not yet played.

## Reply audio on the Echo (2026-11-18)

Measured on the Echo with an instrumented armv7 `echo-bridge` at 34cbe231 and hassmic aaf1ca0 in `-D` mode,
`chatgpt` realtime call over Wi-Fi. Every reply now records these values in a `reply.quality` trace record
(`arrival_gap_max_ms`, `late_arrivals`, `underruns`, `underrun_max_ms`, `depth_min_ms`, `depth_max_ms`,
`discarded.{after_reply,idle,cut,audible}`, `rtp_lost`; since the adaptive playout also
`target_ms.{min,max,final}`, `reprimes`, `concealment_ms`, `fade_ins`, `drained_frames`; since 2026-11-19 also
`cuts.{count,trigger}` and `cuts.holds`). Depth and underruns
are the bridge's model of the device queue (20 ms of playout per frame written); hassmic logs no underrun of
its own.

| Fact | Value |
| --- | --- |
| Call frame arrival at the bridge, 2,065 frames over 41.1 s | mean gap 19.9 ms, longest 62 ms; 7 gaps over 40 ms, 108 in 26–40 ms, 165 at or under 15 ms |
| Same arrivals forwarded frame by frame (no prime), 8 replies of 5 s | 41 modelled underruns in 40 s, longest dry spell 40 ms, median depth 21 ms |
| Same arrivals with a 40, 60, 80 or 100 ms prime | 0 underruns; median depth 43, 69, 91 and 119 ms |
| Live run with the 100 ms prime, 4 replies (1,430, 250, 267 and 279 frames) | `late_arrivals` 5, 4, 1, 2; `underruns` 2, 3, 1, 1; `rtp_lost` 0; `dropped_frames` 0 |
| Where the remaining underruns fall | one per `call.receive_reset` (5 resets, 7 underruns); `arrival_gap_max_ms` 162–443 spans the reset window |
| Audible frames discarded while a reply was being cut | 0, 20, 9, 8 per reply (`discarded.cut` equal); cause not yet determined |
| Cause of those discards (2026-11-19, traces pulled from the Echo) | each user transcript fragment during a reply cut it at once, while the model talked on; playback resumed at the next assistant fragment, 117–353 ms later |
| `arrival_gap_max_ms` in a reply with cuts, same traces | the retired stretch, not the network: 13,072 ms beside 660 `discarded.cut` frames (13.2 s), 1,178 ms beside 67, 480 ms beside 21 |
| `arrival_gap_max_ms` in a reply without a cut, same traces | 64–311 ms, in replies up to 49 s long; `rtp_lost` 0 after each call's first reply |
| RTP sequence gaps | 0 in every measured reply |

The pacing unit test `bridge_pacing_measured_jitter_never_runs_the_primed_echo_dry` replays the measured
gap pattern and fails if a primed stream underruns. This is one device, one network and one evening; it
establishes no Wi-Fi-wide bound. Room recordings through the Ring were too noisy to count clicks (ambient
alone scored 16 "dropouts"), so no acoustic before/after count exists.

## Barge-in on the Echo (2026-11-19)

Measured on the Echo from hassmic's boot log and the bridge trace of one owner run: `echo-bridge` at
e707712d, hassmic aaf1ca0 in `-D` mode, a `chatgpt` realtime call. The owner talked over the replies.

| Fact | Value |
| --- | --- |
| Flushes in the run | 9 in 47 s |
| Reply audio each flush found left to discard | 0 to 5,760 bytes (at most 120 ms); `written` equals `before` in all 9 |
| Flush receipt to closed player in hassmic | 1 to 13 ms |
| Audio written to one reply before its flush | 1,279,680 bytes over 27.1 s: real time at 48,000 bytes/s, not a backlog |
| Replacement playback opened after each flush | 1 to 14 ms later (`speaking -> listening -> speaking`) |
| Model's next assistant fragment after a cut (`call.receive_reset`) | 0.1 to 0.6 s later, after 8 of the 9 cuts |
| After the last cut, with no further model audio | hassmic's duplex failsafe ended the open playback 30.04 s later and went idle |
| Bridge after that failsafe | held the run 35 s more, to the response deadline plus the played timeout, then `live.closed` `failed`, `bridge.recovering` `deadline` |
| A wake 13.5 s after the failsafe | not answered: the bridge still owned the dead run |
| `reply.played` and `reply.quality` rows for the run | none: a reply's rows were written only when it ended uncut |

The cuts came late and the flush had nothing to stop; the flush itself is prompt. One run, one room.

Second owner run, `echo-bridge` at 00287e17 (holds on a sign of user speech), from the bridge trace: 12
replies.

| Fact | Value |
| --- | --- |
| Replies cut | 6 of 12, every one with `cuts.trigger` `user_transcript` |
| Holds with no cut (the model talked on) | 2 replies; 74 and 93 silent frames dropped to give the delay back, 0 audible frames lost |
| Replies in which the Echo ran dry | 5 of 12; `underrun_max_ms` 241, 292, 185, 249, 324 |
| Jitter target when those stalls came | `target_ms.min` 100, 220, 180, 180, 100 against arrival gaps of 299, 268, 146, 181, 233 ms |
| `depth_max_ms` | 180 in every reply: the 200 ms lead less one 20 ms frame, by construction |

Speech onset to a silent device is not measurable from this trace: nothing in it marks the onset. On a
scripted T3 timeline (`bridge_barge_mic.rs`: transcript 700 ms after the speech, loopback device, echo at a
tenth of the reply) it was 785 ms with the transcript as the only sign and 133 ms with the microphone's.
The 700 ms and the echo level are assumptions of that timeline, not measurements of the Echo.

Third owner run, `echo-bridge` at c2844e60 (the microphone holds; cushion kept 120 s): one call, 3 replies.

| Fact | Value |
| --- | --- |
| Reply 1 (first of the call) | `underruns` 1, `underrun_max_ms` 185, `reprimes` 2, `arrival_gap_max_ms` 262, `late_arrivals` 6, `rtp_lost` 4, `target_ms.min` 100 |
| Reply 2 | 93 frames received, 0 played; `cuts` 1 by `user_transcript`, `hold_ms` 1748. hassmic: playback opened, `written=0`, flushed 1.9 s later |
| Reply 3 | 462 frames received, 455 played; cut by `mic_speech` with `hold_ms` 12 |
| Replies 2 and 3, arrival | `arrival_gap_max_ms` 29 and 37, `late_arrivals` 0, `underruns` 0, target held at 300 ms |
| Microphone | `coupling` 4855 per mille; `echo_max` equals `level_max` in all three (15576, 6354, 5788); `onsets` 1, 0, 1; `lead_ms` null |
| Echo load during the run | load average 14; audio HAL 72% and hassmic 62% of one core, of eight |

Fourth owner run, same build, the owner mostly silent: one call, 5 replies, all cut.

| Fact | Value |
| --- | --- |
| Cuts | 5 of 5 by `user_transcript`; over this run and the third, 7 of 8 |
| Microphone speech onsets under those replies | 0 in all 5 |
| Frames played of received | 104 of 175, 107 of 214, 185 of 282, 0 of 30, 12 of 99 |
| `hold_ms` | 1305, 2045, 2002, 611, 1637 |
| Loudest microphone frame under each reply (RMS) | 7214, 12950, 7600, 5595, 15104 |

The session transcribed the Echo's own speaker as the user and interrupted its replies. The transcripts
were not in the trace; `reply.quality` `cuts.text` now carries them.

T3 on the scripted timeline with the session as the only judge (`bridge_barge_mic.rs`): from the session's
transcript to a silent device 187 ms (`cuts.silent_ms` 179, the 200 ms lead less one frame); from the
transcript to the cut 796 ms (`cuts.cut_ms`), which is the model talking on for 300 ms and the 500 ms
quiet stretch. A voice at the microphone alone holds nothing.

## Milestones

| Fact | Value | Measured |
| --- | --- | --- |
| M0 repository harness | Admitted at main `c9a044e`. All 15 T0 gates run in CI and are green on main (run 43748406902). Each gate was seen to fail on a seeded violation in its own lane's PR: F1 15, F2 18, F6 19, F7 20, F4 21, F8 22, F3 23, F9 27, F10 30 (hosted seeds for format, shape and coverage). `echo-lab check --milestone M0` exits 0. The Phase F loopback re-ran every seed from a fresh clone; four narrower gaps it found run as batch `m0-gaps-c9a044e-20261115-v1` alongside W0 (see `decisions.md`) | 2026-11-15, coordinator, from the PRs' seeded-proof comments and the Phase F loopback |
| B0 bench, stock Echo | `echo-lab run` with the device driver passed at T5 on main `7a78937` with port main `179c4ab`: hold, play "Computer, what time is it?", wait for `playEndedAt`, snapshot, record 10 s, release. Whisper `small.en` heard "The time is 6.03 pm." and the time oracle matched. Mouth, ear and STT are proven by the tool, not by hand. Whisper writes the time with a dot (`6.01`), so the scenario's time pattern accepts `[:.]`. Eye: partly. Frames 1–3 s after the question show the Echo's screen wake and show the clock; the frame taken just after `playEndedAt` shows it still dark. The listening bar at the screen's bottom edge is out of frame, and the camera does not tilt lower (`step-ptz DOWN` reported −15° with an unchanged view). Proving the bar needs the camera or the Echo moved by hand | 2026-11-15, coordinator, runs at 18:01 (failed only on the dot) and 18:02 (passed) |
| M1 on the Echo, without the mute steps | `echo-lab run --driver device --bench ring` passed at T5 at 03:27 local with echo-lab and echo-bridge built from main `bc9a5e1` plus the `--allow-quiet-hours` change, port play-audio through the Ring camera, and an `adb forward` of Wyoming 16700. Scenario: `m1-hassmic-bridge.yaml` with its two mute presses and the muted `say` left out (owner decision, `decisions.md` 2026-11-17). Each `say` played one 3.0 s 48 kHz wake clip, spoken "Computer. What time is it?". Results: 22 of 22 acoustic wakes started a Wyoming run (`wyoming.run_started`, `wyoming.mic_frames` at least 10, `wyoming.run_ended`); 10 of 10 bridge restarts and 10 of 10 hassmic-host restarts recovered; 0 bridge errors; 408 s wall. The clip matters. A local VibeVoice voice (`you models invoke tts`), upsampled to 48 kHz, padded 0.3 s and compressed to about −13 dBFS RMS, gave hassmic a confirmed wake (`wake: COMPUTER type=2`) on 2 of 2 test plays. The earlier recorded clip woke the Echo once and then gave only an unconfirmed `type=0` candidate, failing a run at its second `say`. Generic Windows text-to-speech is not used: the owner reports that the wake word rejects it. Mute (`mic.mute_window`) is still unproven at T5 | 2026-11-17, coordinator, runs at 03:19 (failed at the second `say`) and 03:27 (passed) |
| W4 first on-device turn (2026-11-18) | The whole voice path ran on the Echo with no PC in it. `echo-bridge` was the armv7 build of main `77e4c16e` (`make build-echo`), started beside hassmic and reaching Wyoming at `127.0.0.1:16700`; its child was our armv7 `codex-app-server` (`make build-codex-armv7`, a standalone app server, not the `codex` multi-tool). One wake: earcon, GPT-Live connected in about 1.9 s, reply played. Reply: 543 frames, 0 dropped. Microphone: 1405 chunks, 0 dropped. Three workarounds made the run possible: a shell wrapper that removed the `app-server` argument, a wrapper that exported `SSL_CERT_FILE`, and `umask 077` in the launcher, since `adb shell` runs with umask 000 and the audit directory and trace came out world-writable. `codex.launch`, `codex.ca_file` and owner-only creation replace them. A bridge left in the adb session's process group received SIGTERM about 17 s after launch; started with `setsid` it kept running. This is one turn, not an M1 or M4 pass | 2026-11-18, the coordinator's run on the Echo; trace counters |
| W4 egress on the Echo | The bridge reaches the internet when it runs with gid 3990, hassmic's egress group (`runas nobody 3990,inet` for current launches; the original measurement used root). hassmic's firewall chain already lets that gid out, so no firewall rule was added or changed | 2026-11-18, the same run |
| W5 existing identity probes | SELinux Enforcing; adb shell uid 0, context `u:r:su:s0`. Helper grammar: `runas [-r GROUP] [-Z CONTEXT] USER GROUP[,GROUP...] CMD [ARGS...]`; only named users accepted (numeric 3991 returns “unknown user 3991”). Existing nobody uid is 9999. As nobody (and shell) with groups 3990,inet, TCP to 127.0.0.1:16950 and chatgpt.com:443 works; without gid 3990 it is blocked by egress policy. Creating a file in hassmic and listing the legacy root-owned 0700 private directory are denied; /system is read-only. `id` cannot name gid 3990, harmless. These probes do not prove migrated bridge/child identity or wake | 2026-11-19, supplied coordinator measurements; not rerun by this lane |
| W4 bridge as plain root | Started as root without gid 3990, the bridge reached Wyoming on loopback and then got a network deadline: the firewall chain drops its outbound traffic. Being root is not enough; the gid is what the chain matches | 2026-11-18, the same run |
| Reply pacing on the Echo after PR 224 (2026-11-18, main `85a3e770`, bridge on the Echo) | One 10.2 s reply, from `reply.quality`: 510 frames played of 525 received, 0 dropped; 1 underrun (max 102 ms); `arrival_gap_max` 279 ms; `late_arrivals` 3; depth 0–179 ms; `rtp_lost` 5; discarded: audible 0, `after_reply` 15. Microphone: 1363 chunks received, 0 dropped. Before PR 224 the model counted 41 underruns in 40 s of reply. The one remaining underrun follows a 279 ms network gap | 2026-11-18, the coordinator's run on the Echo; trace `reply.quality` |

## Bridge on the Echo: cross-build

`make build-echo` targets `armv7-linux-androideabi` in release mode with Rust 1.100.0 and the installed NDK's
`armv7a-linux-androideabi24-clang` (API 24). Run it on Linux or WSL after provisioning the pinned Rust target
and locked dependency cache. `ANDROID_NDK_HOME` selects the NDK; `ANDROID_NDK_LATEST_HOME` is the fallback
when the first variable is unset or empty. An invalid explicit selection fails. The command downloads nothing.
The bundled Opus build uses the same NDK with `ANDROID_PLATFORM=android-24`; existing Rust crypto features stay
unchanged.

`ECHO_PACKAGES` in the Makefile is the package list: echo-contracts, echo-config, echo-wyoming, echo-audio,
echo-live, echo-chatgpt-auth, echo-agent-api, echo-agent-loop, echo-mcp-auth, echo-mcp-client,
echo-screen-contract, echo-screen-server, echo-delegation and echo-signin, plus echo-bridge. The bridge
manifest is present at this change's base, so the job builds and inspects its real release binary.
echo-link joins when its manifest lands on main. Executable architecture, Android interpreter, binary
size and upload require the matching hosted artifact evidence; configuration alone does not prove them.

The every-PR `build-echo` job logs the installed NDK revision and compiler versions. When echo-bridge exists,
it checks ELF32, ARM, EABI5, `/system/bin/linker` and dynamic dependencies (rejecting OpenSSL), prints its size
and uploads the release binary.

Measured on main at e8fb346, CI run 43827230421 (all jobs green). All 15 packages above compiled for
`armv7-linux-androideabi` (release), with NDK r27d (13750724). `echo-bridge` is an ELF 32-bit LSB PIE executable,
ARM EABI5, interpreter `/system/bin/linker`, for Android 24, unstripped, 10,440,840 bytes. The artifact was
downloaded and checked independently. This establishes no device-runtime or T5 result.
