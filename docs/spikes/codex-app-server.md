---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: research brief read from source; nothing in it was run against a real account. Inputs to phase W2d
---

# Codex app server as the portos-echo backend: research brief

Source: openai/codex `origin/main` at `5b3d2c7fce`, read in a temporary worktree. Unless stated otherwise, all paths are
relative to `codex-rs/`. Line numbers refer to that commit. "Unknown" means not established from the code or from a
local run. Nothing here was signed in, no realtime session was started, and no cross build was run.

Installed reference binary (used only for `--help` and schema generation): codex-cli 0.160.0 on Windows.

---

## 1. App-server realtime API

**Wire format**

- The wire format is newline-delimited JSON-RPC-like messages with no `"jsonrpc"` field. See
  `app-server-protocol/src/rpc.rs:1-2`
  ("We do not do true JSON-RPC 2.0").
- The handshake is `initialize` with `{clientInfo{name,title,version}, capabilities{experimentalApi,
  optOutNotificationMethods, ...}}`
  (`app-server-protocol/src/protocol/v1.rs:29-75`), followed by an `initialized` notification
  (`protocol/common.rs:2206`).
- Every realtime method is `#[experimental]`:
  - Requests are rejected with "requires experimentalApi capability" unless `capabilities.experimentalApi: true`
    (`app-server-protocol/src/experimental_api.rs:30-32`, `app-server/src/message_processor.rs:1043-1046`).
  - Experimental notifications are dropped for clients that did not opt in (`app-server/src/transport.rs:105-125`).

**Requests** (`app-server-protocol/src/protocol/common.rs:1137-1184`)

| Method | Params (`protocol/v2/realtime.rs`) | Core op |
|---|---|---|
| `thread/realtime/start` | `ThreadRealtimeStartParams` 297-375; response is empty, 405-409 | start (`turn_processor.rs:1290-1404`) |
| `thread/realtime/appendAudio` | `{threadId, audio: ThreadRealtimeAudioChunk}` 411-418 | `Op::RealtimeConversationAudio` (`turn_processor.rs:1406-1502`) |
| `thread/realtime/appendText` | `{threadId, text, role?}` (role defaults to user) 426-435 | `...Text` |
| `thread/realtime/appendSpeech` | 443-450 | `...Speech` |
| `thread/realtime/stop` | 458-464 | `...Close` |
| `thread/realtime/listVoices` | 472-484 | n/a |
| `thread/realtime/list`, `thread/timeline/list` | `common.rs:1167` area | not present in the 0.160.0 schema |

- `ThreadRealtimeStartParams` requires `threadId` and `outputModality`.
- Its optional fields are `transport`, `version`, `model`, `voice`, `prompt`, `includeStartupContext`,
  `initialItems` (V3 only, at most 128 items and 8192 tokens), `clientManagedHandoffs`, `delegationAckFiller`,
  `flushTranscriptTailOnSessionEnd`, `codexResponsesAsItems`, `codexResponseItemPrefix`, `codexResponseHandoffMode`,
  `backendReasoningStatus`, `codexResponseHandoffChannelPrefixes`, `realtimeStartInstructions`,
  `realtimeEndInstructions`, `realtimeSessionId` and `voiceSessionId` (`protocol/v2/realtime.rs:297-375`).
- Realtime start goes through turn admission (`app-server/src/message_processor.rs:1072`), so a thread must exist first.
  Create it with `thread/start`, whose params are at `protocol/v2/thread.rs:80-153`.

**Audio chunk**

- The chunk is `{data (base64 PCM16), sampleRate, numChannels, samplesPerChannel?, itemId?}`
  (`protocol/v2/realtime.rs:247-257`).
- Only `data` is forwarded upstream. `sampleRate` is not used to resample
  (`codex-api/src/endpoint/realtime_websocket/methods.rs:292, 362-372`).
- The upstream session is configured for `audio/pcm` at 24000 Hz:
  - Shared constant: `methods_common.rs:26` (`REALTIME_AUDIO_SAMPLE_RATE = 24_000`).
  - V1: `methods_v1.rs:57-66`.
  - V2: `methods_v2.rs:88-112`.
- The Echo captures at 16 kHz, so the bridge must resample to 24 kHz.

**Notifications** (`common.rs:2152-2173`; payloads at `protocol/v2/realtime.rs:486-591`)

The 11 notifications are `thread/realtime/started`, `itemAdded`, `item/started`, `item/transcript/delta`,
`item/completed`, `transcript/delta`, `transcript/done`, `outputAudio/delta` (557-564), `sdp` (566-573),
`error` (575-582) and `closed` `{threadId, reason?}` (584-591).

Mapping from core events (`app-server/src/bespoke_event_handling.rs`):

| Core event | Becomes | Lines |
|---|---|---|
| started, sdp | `started`, `sdp` | 464-481 |
| speech started | `itemAdded {type:"input_audio_buffer.speech_started"}` | 527-539 |
| transcript delta/done | `transcript/delta` / `transcript/done` with role `user` or `assistant` | 541-587 |
| `AudioOut` | `outputAudio/delta` | 589-598 |
| `response.cancelled` | `itemAdded` | 601-613 |
| `ResponseDone` | ignored (not forwarded) | 615 |
| handoff request | `itemAdded` | 628-643 |

**Protocol versions** (`protocol/src/protocol.rs:1969-1974`; default V2)

- **V1**: "quicksilver", `?intent=quicksilver`, header `openai-alpha: quicksilver=v1`.
- **V2**: the standard Realtime API. The default model is `gpt-realtime-1.5` (`core/src/realtime_conversation.rs:116`).
  It uses
  server VAD with `interrupt_response: true` (`methods_v2.rs:102`).
- **V3**: `FramelessBidi`. The default model is `gpt-live-1.1-codex` (`core/src/realtime_conversation.rs:117`), with
  header
  `openai-alpha: quicksilver=v2` (`realtime_conversation.rs:2156-2200`).
  - Events: `session.started/updated`, `output_audio.delta`, `input_transcript.added`, `output_transcript.added`,
    `turn.done`, `delegation.created`, `error`
    (`codex-api/src/endpoint/realtime_websocket/protocol_frameless_bidi.rs:12-95`).
  - Outbound: `input_audio.append`, `delegation.context.append`, `session.context.append`
    (`methods_frameless_bidi.rs:13-100`, `methods.rs:362-372`).

**Version and transport defaults** (`core/src/realtime_conversation.rs:1472-1673`)

- The transport defaults to `websocket` (1518).
- The version defaults to `config.realtime.version` (V2) for websocket, and to **V1** for webrtc and existingCall
  (1541-1545).
- The AVAS architecture requires V1 or V3 (1546-1568).
- Consequence: a V3 webrtc session must send `version: "v3"` explicitly.

**Barge-in and turn end**

- V2: the server VAD interrupts, core sends `conversation.item.truncate` itself (`realtime_conversation.rs:2896`), and
  the
  client sees `itemAdded` for speech_started and response.cancelled.
- V3: no explicit barge-in event is forwarded. How V3 signals barge-in is **unknown**.
- Turn end: `response.done` is not forwarded (`bespoke_event_handling.rs:615`), so use `transcript/done` with role
  `assistant`.
  V3 `turn.done` is parsed in `protocol_frameless_bidi.rs`; how it surfaces as a notification is not traced
  (**unknown**).
- Stop: `thread/realtime/stop` produces `closed`. Delegated Codex work is cancelled with `turn/interrupt`
  (`common.rs:1132`).

**Divergence from portos-echo `docs/program/plan.md`**

- plan.md:45-47 says the delegation event is `session.delegation.created` and that there is no
  `delegation.context.append`. Codex main uses `delegation.created` and sends `delegation.context.append` for V3
  (`methods_common.rs:56-73`, `methods_frameless_bidi.rs:13-100`).
- plan.md:165/212/227 name `gpt-live-1-codex`. Codex main defaults to `gpt-live-1.1-codex`.

## 2. Transport inside

The transport enum is at `protocol/v2/realtime.rs:386-403`: `websocket` | `webrtc {sdp}` | `existingCall {callId}`.

**websocket**

- The app server connects to `wss://api.openai.com/v1/realtime...` (`methods.rs:67, 1241-1291`) using tokio-tungstenite
  with
  rustls (`methods.rs:1120ff`; workspace `Cargo.toml:546-549`).
- All audio flows through JSON-RPC: `appendAudio` in, `outputAudio/delta` out.
- This transport **requires API-key auth** (`realtime_conversation.rs:1586`). `realtime_api_key` fails with "realtime
  conversation requires API key auth" under ChatGPT auth; its only fallback is the `OPENAI_API_KEY` environment variable
  (`realtime_conversation.rs:2130-2154`).
- A TODO at 2143 calls the current ChatGPT/SIWC handling temporary.

**webrtc {sdp}**

- The client supplies its own SDP offer. The app server creates the call (`realtime_conversation.rs:792`,
  `core/src/client.rs:947-1000`) with the provider's auth, so ChatGPT works (`client.rs:835-905`).
- Backend call creation:
  - `POST {chatgpt base}/realtime/calls` with JSON `{sdp, session}` (`codex-api/src/endpoint/realtime_call.rs:45-48,
    63-80, 130-216`).
  - The voice defaults to Cove (143) and the session id is stripped (149).
  - V1, or backend FramelessBidi, adds `?intent=quicksilver&architecture=avas` (218-229).
  - The call id comes from the `Location` header (264+).
- The SDP answer is returned to the client as `thread/realtime/sdp`.
- The app server then opens a reconnecting **sideband** websocket to the call (`realtime_conversation.rs:803`;
  `core/src/realtime_conversation/sideband.rs:25-120`; `methods.rs:922-1067, 1069-1081`).
  - V3 URL: `/v1/live/{call_id}`. V1/V2 URL: `?call_id=` (`methods.rs:1293-1345`).
  - A 404 is retried after 100, 300 and 800 ms.
- **Media goes peer-to-peer between the client's WebRTC stack and OpenAI**, not through the app server. The client must
  run a WebRTC peer with Opus.
- Whether `appendAudio` or `outputAudio/delta` also work over the sideband in webrtc mode is **unknown**.
- AVAS webrtc requires conversational output (`realtime_conversation.rs:1675-1690`).

**existingCall {callId}**

- The client creates the call and the app server only attaches the sideband (`realtime_conversation.rs:822`).
- It rejects `includeStartupContext`, `prompt`, `initialItems`, `model`, `voice` and `delegationAckFiller`
  (`turn_processor.rs:1290-1404`).
- This is what the TUI does:
  - `tui/src/realtime_call.rs:35-106` creates the call with `gpt-live-1.1-codex`, FramelessBidi and
    `openai-alpha: quicksilver=v2`.
  - `tui/src/app_server_session/realtime.rs:20-61` starts with `transport: existingCall`, `version: V3`,
    `clientManagedHandoffs: true` and `includeStartupContext: false`.
- The media peer for the TUI is the separate `voice-host` crate (webrtc 0.20.3, cpal, gstreamer, opus, rubato; framed
  JSON over pipes; see `voice-host/Cargo.toml`, `voice-host/README`). It is not yet wired into the TUI.

**Request headers** (`realtime_conversation.rs:2156-2200`; `codex-api/src/requests/headers.rs:8-33`)

- `openai-alpha`, `x-session-id`, `voice-session-id`, `originator`, and `Authorization`.
- `Authorization` is sent directly only on the websocket transport.
- Call creation may add `x-oai-attestation` (`client.rs:947-1000`).

**Base URLs**

- Overrides: `experimental_realtime_ws_base_url`, `experimental_realtime_webrtc_call_base_url`,
  `experimental_realtime_ws_model` and `experimental_realtime_ws_backend_prompt` (`config/src/config_toml.rs:460-475`).
- The sideband base defaults to `https://api.openai.com/v1` (`realtime_conversation.rs:760`).
- The app server passes `sideband_base_url: None` (`message_processor.rs:1794-1797`).
- With ChatGPT auth the provider base is `https://chatgpt.com/backend-api/codex` (`model-provider-info/src/lib.rs:87,
  475-486`).

**Queueing**

- The audio-in queue holds 256 frames and uses `try_send`, so frames are **dropped when it is full**
  (`realtime_conversation.rs:102, 976-1001`).
- Input loop: `realtime_conversation.rs:2393-2500`. Audio passthrough: 3023-3033.

## 3. Tools and handoff

**Delegation path**

- A server `delegation.created` (target `client`) or a V1 handoff request becomes `HandoffRequested`.
- The fanout task (`realtime_conversation.rs:1975-2010`) routes it through `route_handoffs.route` to
  `session.route_realtime_text_input` (`core/src/session/turn_input.rs:922-972`).
- That submits `UserInput::Text` as a normal Codex turn with `StartOrSteer` and `turn_trigger: "realtime"`.
- The turn has **every tool configured for the thread**, including MCP servers.
- Results return to the realtime model through `handle_handoff_output` (`realtime_conversation.rs:2648-2760`):
  - StandaloneSpeech at 2664/2721.
  - Handoff append at 2689/2709/2751.
  - The append becomes `delegation.context.append` on V3 and `conversation.handoff.append` on V1
    (`methods_common.rs:56-73`).
- If `clientManagedHandoffs` is true, `handoff_out` returns early and the client owns the handoff
  (`realtime_conversation.rs:1058-1080`). The client can then use `appendSpeech` (1289-1312); `handoff_complete` is at
  1314-1355.
- The V2 realtime tool list describes `background_agent` and `silence` (`methods_v2.rs:34, 36, 115-143`).

**Controlling which tools are exposed** (config only; the app-server crate has no cargo features)

- **Shell**: skipped when `features.shell_tool = false` or the model shell type is Disabled
  (`core/src/tools/spec_plan.rs:1289-1300`). `shell_tool` is stable and defaults to true
  (`features/src/lib.rs:1198-1201`);
  `unified_exec` defaults to true (1229).
- **apply_patch**: added when `model_info.apply_patch_tool_type` is set (`spec_plan.rs:1483-1486`). No user config knob
  was
  found to disable it, so whether it can be turned off is **unknown**.
- **MCP**: per-server `enabled_tools` / `disabled_tools` (`config/src/mcp_types.rs:293-297, 451-453`).
- **Client-implemented tools**: `dynamicTools` on `thread/start` (experimental, `protocol/v2/thread.rs:80-153`) arrive
  as
  `item/tool/call` requests to the client (`common.rs:1942`).
- **Environments**: setting `environments` empty "disables environment access" (experimental, `thread.rs:80-153`).
- **Other thread settings**: `sandbox`, `approvalPolicy`, `config` map, `baseInstructions` and `developerInstructions`
  are
  set on `thread/start`.
- **Feature flags**: `--enable` and `--disable` exist on `codex app-server` (0.160.0 `--help`). Core tool sources are
  wired at `spec_plan.rs:1231-1251`. `code_mode` is off by default (`features/src/lib.rs:1329`).
- Realtime itself is not feature-gated: `realtime_conversation` is `Stage::Removed` (`features/src/lib.rs:2392-2395`).
  `in_app_voice` is a requirements-only gate (1978-1981).
- On Android there is no platform sandbox (`sandboxing/src/manager.rs:53-67`). If Codex ran on the device, shell tools
  would run unsandboxed.

## 4. Auth

**Login methods** (`account/login/start`, `common.rs:1380`; `LoginAccountParams`, `protocol/v2/account.rs:72-132`,
tagged by `type`)

- The types are `apiKey`, `chatgpt` (browser), `chatgptDeviceCode`, `chatgptAuthTokens` (experimental) and `bedrock`.
- `chatgptDeviceCode` returns `{loginId, verificationUrl, userCode}` (`account.rs:148-178`). Completion arrives as
  `account/login/completed` (`common.rs:2179`; `account.rs:899-906`).
- Related methods: `account/login/cancel` (1415), `account/logout` (1437), `account/read` (1589) and
  `account/chatgptAuthTokens/refresh` (1947).
- The device-code flow lives in `login/src/device_code_auth.rs:68, 107, 165-229`:
  - Endpoints `deviceauth/usercode` and `deviceauth/token`, verification page `{base}/codex/device`.
  - `request_device_code` and `complete_device_code_login` are public and re-exported at `login/src/lib.rs:25-28`.

**Storage and refresh**

- Credentials are stored in `$CODEX_HOME/auth.json` (`login/src/auth/storage.rs:51-77, 168`).
- The mode is `cli_auth_credentials_store` (`config_toml.rs:304`): File (default), Keyring, Auto or Ephemeral
  (`config/src/types.rs:139-149`). There is no Android keyring backend in `keyring-store/Cargo.toml`, so Android
  falls back to the file store.
- Tokens refresh 5 minutes before expiry or after 8 days against `https://auth.openai.com/oauth/token`, which can be
  overridden with `CODEX_REFRESH_TOKEN_URL_OVERRIDE` (`login/src/auth/manager.rs:232-243, 3668-3676`).

**Auth versus transport**

| Transport | ChatGPT auth | API-key auth |
|---|---|---|
| websocket | fails ("requires API key auth"), unless `OPENAI_API_KEY` is set | works (`realtime_conversation.rs:2130-2154`) |
| webrtc / existingCall | works (`client.rs:835-905`, `947-1000`) | works |

`codex-api` exposes an `AuthProvider` trait and `SharedAuthProvider` (`codex-api/src/auth.rs:34-111`), so a library
user can supply the bearer token directly.

## 5. Running it

**Subcommands and flags** (from `codex app-server --help`, 0.160.0; `app-server/src/main.rs:43-50`)

- `--listen stdio://` (default) | `unix://` | `unix://PATH` | `ws://IP:PORT` | `off`; also `--stdio`.
- `--ws-auth capability-token|signed-bearer-token`.
- `-c key=value`, `--enable`, `--disable`, `--strict-config`.
- Subcommands: `daemon` (bootstrap, start, restart, update, enable/disable-remote-control, stop, version), `proxy`,
  `generate-ts` and `generate-json-schema`.
- The stdio transport is a line reader on stdin and a writer on stdout
  (`app-server-transport/src/transport/stdio.rs:60-100`).

**Schema**

- `codex app-server generate-json-schema --experimental --out <DIR>` works offline. It produced the full v2 schema
  including the realtime methods and notifications.
- The 0.160.0 schema lacks `thread/realtime/list`, `thread/timeline/list` and `voiceSessionId`, so main is ahead of the
  release.

**Minimal sequence for a voice session** (from the code paths above; not executed)

1. `initialize` with `experimentalApi: true`, then `initialized`.
2. `account/read`, and if needed `account/login/start {type: chatgptDeviceCode}`.
3. `thread/start` with the tool, sandbox and instruction settings.
4. `thread/realtime/start {threadId, outputModality, transport, version:"v3"}`.
5. Media: either `appendAudio` and `outputAudio/delta` (websocket, API key), or a client WebRTC peer plus
   `thread/realtime/sdp` (webrtc, ChatGPT).
6. `thread/realtime/stop`.

**Disk state**

- `sqlx` with bundled sqlite (workspace `Cargo.toml:514-522`), plus rollouts and auth under `$CODEX_HOME`.
- Disk usage per session is **unknown**. Use `ephemeral` on `thread/start` (`thread.rs:80-153`) to limit it.

**Release binaries** (Windows, for scale only)

- `codex.exe` 0.160.0 is 326,872,368 bytes and `codex-code-mode-host.exe` is 74,697,520 bytes.
- Earlier releases: 0.144.1 about 341 MB, 0.149.0 about 295 MB, 0.156.0 about 323 MB.
- The release profile is thin LTO, `strip = false`, `codegen-units = 4` (workspace `Cargo.toml:662-671`).

## 6. ARM cross-build (armv7 Android / Linux)

**CI and toolchain**

- No armv7 or Android target exists in CI or releases. Releases cover aarch64/x86_64 apple-darwin, x86_64/aarch64
  linux-musl (`.github/workflows/rust-release.yml:102-145`) and x86_64/aarch64 windows (`rust-release-windows.yml`).
  Full CI covers aarch64 gnu/musl/windows/mac (`rust-ci-full.yml:144-229`).
- The workspace version is `0.0.0` (`Cargo.toml:177`).
- There are git patches for crossterm, tokio-tungstenite, tungstenite and v8 (`Cargo.toml:684-700`).
- The repo pins Rust 1.99.1. The local WSL had 1.100.0 with `armv7-linux-androideabi` installed.

**What is already handled for `target_os = "android"`**

- landlock and seccompiler are Linux-only deps (`protocol/Cargo.toml:56-58`).
- `bwrap/build.rs` skips the vendored bubblewrap off Linux.
- `linux-sandbox` is entirely `cfg(target_os = "linux")`. `arg0` depends on it unconditionally (`arg0/Cargo.toml:21`),
  but
  it compiles to stubs on Android.
- Platform sandboxing returns None on Android (`sandboxing/src/manager.rs:53-67`).
- Android cfgs already exist in `process-hardening/src/lib.rs:13-45` and `exec-server/src/no_follow/unix.rs:26`.
- v8 is pulled only by `code-mode-runtime` and `v8-poc`, not by `codex-app-server` (workspace `Cargo.toml:574, 694`).
  code-mode runs out of process in `codex-code-mode-host`.
- `tikv-jemallocator` is used only on musl x86_64/aarch64. `codex-windows-sandbox` is Windows-only
  (`app-server/Cargo.toml`).

**Likely blockers (not verified by building)**

- **native-tls / openssl-sys.** `http-client` depends on `native-tls 0.2` unconditionally
  (`http-client/Cargo.toml:14-16`),
  which is OpenSSL on Android. Vendored openssl is enabled only for x86_64/aarch64 musl (`core/Cargo.toml:136-142`).
  armv7 needs `OPENSSL_DIR`, or a `openssl-sys/vendored` feature added through a patch or a wrapper crate.
- **aws-lc-sys.** rustls uses the `aws_lc_rs` provider (workspace `Cargo.toml:483-486`), which needs the NDK clang plus
  cmake. Whether it builds for armv7 Android is **unknown**.
- **C crates.** libsqlite3-sys (bundled), zstd-sys and bzip2-sys build with `cc`. They should build with the NDK; not
  verified.
- **portable-pty.** It is in the app-server closure; whether it builds on Android is **unknown**.
- **CA roots.**
  - The only Android CA path in the code is a Termux path (`network-proxy/src/native_certs.rs:227`).
  - rustls-native-roots on Android behaviour is **unknown**.
  - Workaround: ship a bundle and set `CODEX_CA_CERTIFICATE` or `SSL_CERT_FILE` (`http-client/src/custom_ca.rs:14,
    63-64`).
- **Keyring.** There is no Android backend, so it falls back to the file store (see section 4).
- A 32-bit target is untested upstream. Atomics, `usize`-sized limits and file offsets on armv7 are **unknown risks**.

**Dependency check attempted**

- `cargo tree --target armv7-linux-androideabi` was abandoned.
  - Offline, it failed on the crossterm git dependency.
  - Online, it began cloning the rusty_v8 fork and its submodules: about 3.6 GB of cargo caches, with no result.
- The crate counts in "Reuse options" come from a static closure over `Cargo.lock` and the member `Cargo.toml` files
  with Android cfg heuristics. They are approximate upper bounds, not `cargo tree` output.

## 7. Licensing

- `LICENSE` is Apache-2.0. `NOTICE` reads "OpenAI Codex, Copyright 2025 OpenAI" and includes the Ratatui MIT
  attribution.
- Redistributing a binary or linking crates requires keeping LICENSE and NOTICE and marking modified files
  (Apache-2.0 section 4).
- `deny.toml` allows, among others, MPL-2.0 (nucleo-matcher) and the OpenSSL license (aws-lc-sys).
- `vendor/bubblewrap` (LGPL) is built only for Linux, so it is not in an Android build.
- Whether the backend endpoints (`chatgpt.com/backend-api/codex/realtime/calls`, `/v1/live`) and the
  `gpt-live-1.1-codex` model may be used by a third-party client under a ChatGPT sign-in is **unknown**. The code
  license
  does not cover it; it is a terms-of-service question.

## 8. Risks

1. **Experimental surface.** Everything realtime is `#[experimental]` and changing quickly. Main is already ahead of
   0.160.0 (new methods and fields). The V3 event names differ from what plan.md recorded.
2. **ChatGPT auth versus websocket.** The simple all-JSON audio path needs an API key. Under ChatGPT auth a WebRTC/Opus
   peer is mandatory in our code, whichever option we pick.
3. **Version default trap.** webrtc and existingCall default to V1, so V3 must be sent explicitly.
4. **No barge-in or turn-end signal for V3** in the app-server notifications (**unknown** handling). `response.done` is
   dropped.
5. **Audio drops.** The 256-frame queue uses `try_send` and drops silently.
6. **Footprint.** A ~970-crate closure, sqlite state and rollouts on disk, an unmeasured RSS, and a 327 MB Windows
   release
   binary for scale.
7. **Unsupported target.** armv7 Android is not built upstream. OpenSSL, aws-lc and CA roots need work. There is no
   sandbox on Android.
8. **Tool exposure.** Delegated turns get every configured tool by default. apply_patch may not be disableable by
   config.
9. **Attestation and headers.** Call creation can carry `x-oai-attestation` (`client.rs:947-1000`). Whether the backend
   will require attestation from third-party clients is **unknown**.
10. **Terms.** Whether a third-party client may use the backend realtime endpoints is **unknown** (section 7).

---

## Reuse options (ranked)

The figures below come from a static closure over `Cargo.lock`, so they are approximate upper bounds. Binary and RSS
sizes are **estimates, not measured**.

| | Closure (approx.) | Stripped armv7 binary (est.) | Idle RSS (est.) |
|---|---|---|---|
| codex-app-server | 973 crates (127 workspace, 846 external) | 60-120 MB | 50-150 MB |
| codex-core | 915 | (part of the above) | |
| codex-app-server-protocol | 757 (pulls core-adjacent types) | | |
| codex-login | 731 | | |
| codex-api | 545 (18 workspace) | 8-20 MB for a bridge built on it | 10-30 MB |
| codex-protocol | 531 | | |
| codex-websocket-client | 244 | | |
| codex-client | 240 | | |
| codex-http-client | 236 | | |

Notable crates in the app-server closure: aws-lc-rs/sys, ring, native-tls, openssl-sys/openssl-src,
libsqlite3-sys/sqlx, zstd-sys, bzip2-sys, cmake, cc, keyring, portable-pty, rmcp, tonic, axum and tokio-tungstenite.

### 1st (recommended): (c) link codex-rs crates in our own bridge

- **Crates**: `codex-api` as the core of it.
  - `endpoint/realtime_call.rs`: call creation with the right body, query and headers.
  - `endpoint/realtime_websocket/methods.rs`: `connect`, `connect_webrtc_sideband`, `connect_existing_call_sideband`
    with 404 retry, URL builders, and `send_audio_frame`.
  - `protocol_frameless_bidi.rs` and `methods_frameless_bidi.rs`: the V3 parser and writer, including
    `delegation.created` and `delegation.context.append`.
  - `auth.rs`: an `AuthProvider` we implement.
- **Optional**: `codex-login` for device code and refresh (`device_code_auth.rs`, `auth/manager.rs`), or reimplement
  those
  two small HTTP flows. codex-login's closure is large (731 crates), so check whether it pulls core before linking it.
- **Avoid**: `codex-app-server-protocol` and `codex-protocol` (531-757 crates) for types alone. Copy the few shapes
  needed.
- **Realtime transport pieces reused**: call creation, sideband websocket with reconnect, event parsing and
  serialization,
  request headers.
- **What we still write**:
  - The WebRTC/Opus media peer. It is needed under ChatGPT auth in every option; on the PC use `webrtc-rs`, which is
    what `voice-host` uses.
  - Resampling between 16 and 24 kHz.
  - The delegation agent loop: running a text model with our own small tool set and MCP, then
    `delegation.context.append`.
  - Barge-in and turn handling.
  - The Wyoming side.
- **Why first**:
  - Smallest weight and binary by roughly an order of magnitude.
  - Exact tool control, because we only expose the tools we implement.
  - No sqlite or rollout state.
  - It reuses the parts that encode the undocumented protocol details (endpoints, headers, V3 event names).
- **Cost**:
  - We own the agent loop, which in (a) and (b) is Codex's turn engine.
  - We track upstream churn in `codex-api` ourselves; it is an internal crate with no API stability promise.
- **Open item**: whether `codex-api` alone cross-builds for armv7. It still pulls `codex-http-client`, so native-tls and
  OpenSSL remain unless they are patched to rustls only.

### 2nd: (b) slimmed app server, the same crate configured down

- **Crates**: identical to (a). `codex-app-server/Cargo.toml` has **no cargo features**, so nothing can be trimmed at
  build time.
- **Tool trimming is config only**:
  - `features.shell_tool = false` and `unified_exec = false` (`spec_plan.rs:1289-1300`).
  - Per-MCP `enabled_tools` / `disabled_tools` (`mcp_types.rs:293-297, 451-453`).
  - `dynamicTools` for client-side tools.
  - Empty `environments`.
  - `--disable` flags.
  - apply_patch control is **unknown** (`spec_plan.rs:1483-1486`).
- **Weight, binary and RSS**: as (a), about 973 crates and an estimated 60-120 MB stripped.
- **Transport pieces reused**: all of the app server's (call creation, sideband, delegation into Codex turns, handoff
  append).
- **What we still write**:
  - The JSON-RPC client.
  - The WebRTC/Opus peer under ChatGPT auth (or the audio path with an API key).
  - Resampling and the Wyoming side.
  - Config hardening.
- A real slimming would mean a fork with cargo features: a maintenance burden with no upstream support.

### 3rd: (a) ship the full cross-built `codex app-server`, talk JSON-RPC

- **Crates**: the full `codex-app-server` closure (about 973 crates), every Codex tool and default feature, and
  `codex-code-mode-host` if code mode is wanted (v8, excluded otherwise).
- **Binary**: a 327 MB Windows release for scale, estimated at 60-120 MB stripped on armv7 (unmeasured).
- **Transport pieces reused**: everything server side.
- **What we still write**: the JSON-RPC client, the WebRTC/Opus peer (ChatGPT auth), resampling and Wyoming.
- **Ranked last** because:
  - It is the largest footprint on a small device.
  - It exposes every Codex tool unsandboxed on Android.
  - It is an unsupported 32-bit target.
  - The only gain over (b) is skipping configuration.
- **Deployment note**: (a) and (b) are reasonable on the operator's PC, inside `echo-bridge`, not on the Echo. There the
  footprint matters less and upstream x86_64/aarch64 builds exist. That would avoid the armv7 cross build altogether.

### Recommendation

Choose **(c)**. Link `codex-api` (realtime call, sideband, V3 parser and writer, our own `AuthProvider`) into
`echo-bridge`, write the WebRTC/Opus peer and a small delegation loop with only the tools we choose, and take
`codex-login` only if its closure stays small (otherwise reimplement device code and refresh).

If the owner prefers maximum reuse of Codex's agent loop over footprint, the fallback is (b) running on the PC rather
than
the Echo, with shell disabled and MCP tool filters.

Either way, first verify that `codex-api` cross-builds for armv7 with OpenSSL vendored or removed. That has not been
done.
