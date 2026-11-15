---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; measured facts, each with its date and source
---

# Measured baselines

Facts measured on the real device, bench and services. Each row has its date and how it was measured. Plans and
factory payloads cite these instead of rediscovering them. Add a row whenever a measurement changes a decision.

## The device

| Fact | Value | Measured |
|---|---|---|
| Model | Echo Show 8 (2nd gen), `ro.product.model` `AEOAT`, device `athena` | 2026-11-15, `adb shell getprop` |
| OS | Fire OS 7.0 (Android 9), build `PS75751.7809N` | 2026-11-15, `getprop ro.build.display.id` |
| ABI | `armeabi-v7a`, 32-bit userland | 2026-11-15, `getprop ro.product.cpu.abi` |
| Memory | about 1.9 GB (`MemTotal` 1973744 kB) | 2026-11-15, `/proc/meminfo` |
| Access | rooted; `adb shell` runs as `uid=0` | 2026-11-15, `adb shell id` |
| Assistant software | stock: `hassmic` (echo-dot-assist) is not installed | 2026-11-15, no `/data/local/hassmic` |
| Wake word models | Pryon models for "Alexa", "Amazon", "Computer" and "Echo" in `/system/local/models/keyword/<locale>/` (21 locales); hassmic supports all four. "Alexa" is the decided wake word | 2026-11-15, `ls /system/local/models/keyword/en-US/` |
| Free memory with stock Alexa running | `MemAvailable` about 670 MB | 2026-11-15, `/proc/meminfo` |
| SELinux | `Enforcing` (the unlock's policy; hassmic's install extends it) | 2026-11-15, `getenforce` |

## Firmware probe of build 7809

Probed 2026-11-15, read only: the same reads as echo-dot-assist's `scripts/device/probe.sh`, run over `adb shell`
without pushing a file, plus `getevent -lp`, `getevent -il`, `/proc/bus/input/devices`, `tinymix`, `dumpsys input`,
`/proc/asound` and `wm size`. Raw output stays in the operator's artifact directory; it holds the device serial.

echo-dot-assist's `devices/athena` profile is pinned to 7080 (`probe.md5`), verified on 7077 and 7082, and was
installed and self-tested on 7806 (2026-11-01), whose `libpryon.so` and `libaudioflinger.so` differ from 7080's.

| File (under `/system`) | 7809 md5 | Same as 7080 |
|---|---|---|
| `lib/libpryon.so` | `5f213d89d39114c6b5d58fd6489b5598` | No |
| `vendor/lib/libasp.so` | `ac5e8985aa9d67461d3b054bde179363` | Yes |
| `lib/libaspclient.so` | `aeb4f3dc3d9ace3157d5c36bf9739836` | Yes |
| `lib/libaudioflinger.so` | `9318b38640edfdba31c3663e202df0d4` | No |
| `vendor/lib/hw/audio.primary.mt8183.so` | `cc54a24e1499090ac648adccae2033dd` | Yes |
| `lib/libwakewordserver_jni.so` | `0926fadcaefc0b8723a6d7d5988cae54` | Yes |

The two that differ are the two that differed on 7806. echo-dot-assist does not record 7806's hashes, so whether
7809's are byte-identical to 7806's is unknown; hassmic's setup will ask before installing (`HASSMIC_FIRMWARE_OK`).
Kernel `4.4.146+`, SDK 28, incremental `0036811211140`.

| Signal | 7809 value | Matches the athena profile |
|---|---|---|
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
|---|---|---|
| Held session listens and talks | `port session hold <ring> --camera --listen` reports `listening: true`, `talking: true` | 2026-11-13 |
| Ambient room level | about −70 dBFS mean over 3 s (`port message record --audio-only`) | 2026-11-13 |
| Speaker to microphone delay | 320 ms from send to first heard, for the full 0.5 s chirp (`play-audio --chirp --listen 3`) | 2026-11-13 |
| Stock Echo answers a question played through the Ring | "Alexa, what time is it?" spoken by the `you` factory's local `tts` model, normalized to −1 dBTP, answered; Whisper `large-v3-turbo` transcribed "The time is 7:49 p.m." | 2026-11-13 |
| Ring microphone during its own playback | sometimes ducked to about −80 dBFS while the Ring speaker plays; record replies after the clip ends | 2026-11-13 |
| Synthetic voices | quiet or robotic Windows voices woke the Echo but were rejected as false wakes; a natural TTS voice at full level worked | 2026-11-13 |
| Held session lifetime | a hold can be preempted when another viewer takes the camera (`reason: preempted`); open a new hold and retry | 2026-11-13 |

Not measured yet: a Ring frame of the Echo's light ring (the "eye" half of milestone B0), and the Ring's lockout
threshold for repeated sessions.

## Services

| Fact | Value | Source |
|---|---|---|
| Port OS MCP transport | stateless streamable HTTP: POST only, no `Mcp-Session-Id`, `GET /mcp` answers 405, no elicitation | Port OS plan review, 2026-11-13 |
| Port OS MCP auth | `/mcp` admits any access token with `token_use=access`, the issuer, the MCP audience, a subject and an active `grant_id`, whatever grant minted it. Authorization code + PKCE (DCR or CIMD) works on the pilot today; device-code tokens work once portos-monolith PR 2731 (merged 2026-11-15) is deployed | Port OS plan review, 2026-11-13; portos-backend coordinator, 2026-11-15 |
| Port OS device-code grant | `POST /auth/device-authorization` needs `resource` = the protected resource's `/mcp` URL (anything else: 400 `invalid_target`; none: a first-party token `/mcp` refuses with 401). Access token 24 h, refresh token 30 days bound by `cnf.jkt`; refresh tokens rotate and a replay revokes the grant. The built-in public CLI client already allows `device_code` and `refresh_token`; DCR and CIMD clients cannot use the device flow. Code lifetime 1800 s, poll interval 5 s | portos-backend coordinator, 2026-11-15 (PR 2731); `docs/user-guides/oauth/oauth-device-auth.md` |
| Port OS pilot device grant, live | 2026-11-15, from a fresh client: `POST /auth/device-authorization` (built-in CLI client, `endpoint:read`, `resource` = `https://api.portoperatingsystem.com/mcp`, ES256 DPoP proof), approved in a browser, then the `device_code` grant returns `token_type` Bearer, `endpoint:read` and a refresh token. `/mcp` `initialize` 200 and `tools/list` 200 with 10 tools (`control`, `endpoint_*`, `group_*`, `subscription_*`) on plain Bearer with no proof; `refresh_token` with the same key 200. Discovery on the auth and api origins advertises `device_authorization_endpoint` `https://auth.portoperatingsystem.com/auth/device-authorization`. PB-4 is deployed (portos-monolith PR 2735): a proof whose `htu` names `https://auth.portoperatingsystem.com/auth/token`, sent to `/auth/device-authorization`, now answers 400 (200 before PB-4), and the whole flow above still passes with a proof signed for each exact endpoint URL. Query strings in `htu` are ignored and the `Host` header is never trusted | portos-backend coordinator; first run on the pilot image from main `8b7b405`, PB-4 rerun on main `35aa60d` |
| Port OS pilot access token lifetime | 300 s, so a 30-minute run refreshes about six times | Port OS pilot runbook |
