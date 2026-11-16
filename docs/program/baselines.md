---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: active; measured facts, each with its date and source
---

# Measured baselines

Facts measured on the real device, bench and services. Each row has its date and how it was measured. Plans and
factory payloads cite these instead of rediscovering them. Add a row whenever a measurement changes a decision.

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

## Milestones

| Fact | Value | Measured |
| --- | --- | --- |
| M0 repository harness | Admitted at main `c9a044e`. All 15 T0 gates run in CI and are green on main (run 43748406902). Each gate was seen to fail on a seeded violation in its own lane's PR: F1 15, F2 18, F6 19, F7 20, F4 21, F8 22, F3 23, F9 27, F10 30 (hosted seeds for format, shape and coverage). `echo-lab check --milestone M0` exits 0. The Phase F loopback re-ran every seed from a fresh clone; four narrower gaps it found run as batch `m0-gaps-c9a044e-20261115-v1` alongside W0 (see `decisions.md`) | 2026-11-15, coordinator, from the PRs' seeded-proof comments and the Phase F loopback |
| B0 bench, stock Echo | `echo-lab run` with the device driver passed at T5 on main `7a78937` with port main `179c4ab`: hold, play "Computer, what time is it?", wait for `playEndedAt`, snapshot, record 10 s, release. Whisper `small.en` heard "The time is 6.03 pm." and the time oracle matched. Mouth, ear and STT are proven by the tool, not by hand. Whisper writes the time with a dot (`6.01`), so the scenario's time pattern accepts `[:.]`. Eye: partly. Frames 1–3 s after the question show the Echo's screen wake and show the clock; the frame taken just after `playEndedAt` shows it still dark. The listening bar at the screen's bottom edge is out of frame, and the camera does not tilt lower (`step-ptz DOWN` reported −15° with an unchanged view). Proving the bar needs the camera or the Echo moved by hand | 2026-11-15, coordinator, runs at 18:01 (failed only on the dot) and 18:02 (passed) |

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
