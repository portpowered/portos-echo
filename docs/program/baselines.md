---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; measured facts, each with its date and source
---

# Measured baselines

Facts measured on the real device, bench and services. Each row has its date and how it was measured. Plans and
dispatch briefs cite these instead of rediscovering them. Add a row whenever a measurement changes a decision.

## The device

| Fact | Value | Measured |
|---|---|---|
| Model | Echo Show 8 (2nd gen), `ro.product.model` `AEOAT`, device `athena` | 2026-11-15, `adb shell getprop` |
| OS | Fire OS 7.0 (Android 9), build `PS75751.7809N` | 2026-11-15, `getprop ro.build.display.id` |
| ABI | `armeabi-v7a`, 32-bit userland | 2026-11-15, `getprop ro.product.cpu.abi` |
| Memory | about 1.9 GB (`MemTotal` 1973744 kB) | 2026-11-15, `/proc/meminfo` |
| Access | rooted; `adb shell` runs as `uid=0` | 2026-11-15, `adb shell id` |
| Assistant software | stock: `hassmic` (echo-dot-assist) is not installed | 2026-11-15, no `/data/local/hassmic` |
| Wake word | Pryon "Alexa" from the firmware's `libpryon`; echo-dot-assist's default and the only keyword the firmware carries | echo-dot-assist `src/hassmic/main.c`, wake word models comment |

**Firmware probe status.** echo-dot-assist's `devices/athena` profile was verified on builds 7077–7082 and on
7806. This device runs 7809, which nobody has probed. A fresh probe (`getevent -il`, `getevent -lp`,
`/proc/bus/input/devices`, `tinymix`, `dumpsys input`) must be recorded here before a hardware adapter runs on it.

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
| Port OS MCP auth | Bearer access tokens from authorization code + PKCE only; the device grant does not reach MCP yet | Port OS plan review, 2026-11-13 |
| Port OS pilot access token lifetime | 300 s, so a 30-minute run refreshes about six times | Port OS pilot runbook |
