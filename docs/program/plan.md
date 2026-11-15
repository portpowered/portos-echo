---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; the program plan. It wins over the drafts in docs/design/
---

# Port OS Echo: program plan

This is the plan the coordinator works from. It starts from the two drafts in `docs/design/` (the RFC and the
multi-agent execution plan) and corrects them where a review against the real services, the real device and the
real bench found them wrong. Where this plan and a draft disagree, this plan wins.

How the work is run (the coordinator and the you-agent-factory) is in `docs/program/coordinator.md`. The standing
rules are in `docs/program/rules.md`. Measured facts are in `docs/program/baselines.md`. Decisions and open
questions are in `docs/program/decisions.md`.

## 1. What we are building

A rooted Echo Show 8 (2nd gen, `athena`) becomes a voice front end for GPT-Live that can act on the home through
Port OS:

1. On the Echo, echo-dot-assist's `hassmic` hears the wake word with the firmware's Pryon engine (section 5),
   captures the room through Amazon's echo canceller and plays replies. It runs in Wyoming mode with `-D`, which
   keeps the microphone open while a reply plays: the duplex mode echo-dot-assist added for realtime clients
   (#173).
2. On the operator's PC, `echo-bridge` connects to hassmic over Wyoming, carries the audio to and from a GPT-Live
   session, runs the delegated agent loop and executes every MCP tool call: against Port OS MCP for the home, a
   spoof MCP for tests, and later other MCP servers.
3. A small screen client on the Echo shows what the bridge is doing: status, the Port OS link code, approval
   prompts and the M6 animation.

Owner decision (2026-11-15): this is the hassmic `-D` route. It replaces the drafts' on-device stack (a C hardware
shim, a Rust daemon on the Echo, an on-device WebRTC peer and an on-device MCP client). What the drafts got right
stands: one delegated agent owns MCP, the credentials stay on the owner's side, and milestones are proven on the
device.

Premises the review found wrong, and what this plan does instead:

1. **A server-side peer is required in every topology.** GPT-Live creates a session from your server with your
   credential (a Platform API key, or the ChatGPT sign-in OpenClaw uses, section 4), and neither belongs on a
   rooted device. With the `-D` route the bridge is that server and the WebRTC peer itself: audio flows Echo ↔
   bridge over Wyoming, and bridge ↔ OpenAI over WebRTC.
2. **The drafts' GPT-Live event names are wrong.** Delegation arrives as `session.delegation.created` with
   `delegation.target` set to `client` or `responses`. Results go back with `session.commentary.append`. There is
   no `delegation.context.append`. Contracts are generated from captured traces, never from the RFC.
3. **Port OS MCP does not yet accept device-code tokens.** `/mcp` accepts Bearer tokens from authorization code +
   PKCE today. Making it accept any valid Port OS access token for the MCP resource, whatever grant minted it, is
   portos-backend work (section 9).
4. **Muse is reached through a wrapper around the Muse gadget SDK,** with an explicit callback command for
   correlation. Muse never receives the bridge's credentials (section 7).
5. **The bench is a Ring camera driven by the Port OS CLI.** One held Ring session plays utterances into the room,
   records the reply and watches the screen. It works today on a stock Echo (`docs/program/baselines.md`).
6. **The hardware work is already done upstream.** hassmic drives athena's audio HAL, Pryon, the mute latch, the
   keys and the screen, and echo-dot-assist tests it on the host against `hassmic-host`. We consume it at a pinned
   commit and send any change we need upstream as a pull request.

## 2. Architecture

```text
Echo Show (stock firmware + hassmic)            echo-bridge (Rust, operator's PC)        Services
  hassmic: Pryon "Alexa", AEC, mic,   <-Wyoming->  Wyoming client, audio framing
  speaker, mute latch, keys (-D)                   GPT-Live peer (WebRTC + sideband) <==> GPT-Live (OpenAI)
  echo-screen: WebView of the bridge  <--HTTP-->   screen server (UiState, UiAction)
  page (status, prompts, M6)                       agent loop + MCP clients ---------->  Port OS MCP, spoof MCP
                                                   Port OS OAuth (device code, DPoP)
```

| Component | Runs on | Holds | Does |
|---|---|---|---|
| `hassmic` (echo-dot-assist, pinned) | The Echo | Nothing of ours | Wake word, echo canceller, capture and playback, mute latch, keys, light band. `PROTO=wyoming`, `ARGS` with `-D`. Installed by echo-dot-assist's `scripts/install-system.sh` |
| `echo-screen` | The Echo, as hassmic's home app | A pairing token for the bridge's screen page | Shows the bridge's screen page full screen; touches go back to the bridge as `UiAction`s |
| `echo-bridge` and its crates | The operator's PC first (section 6) | The GPT-Live credential; the Port OS grant (DPoP key, refresh token, access token) | Wyoming client; the GPT-Live session; the delegated agent loop; every MCP call; `session.commentary.append`; the screen server; caps, audit log, kill switch |
| `echo-lab` | The operator's workstation and CI | Bench session leases (never committed) | Runs scenario files with the `sim`, `cloud` or `device` driver and writes reports |
| `echo-spoof-mcp` | CI and the bench | Nothing secret | Deterministic MCP server: `test.echo`, `test.wait`, `test.requires_approval`, `ui.show_emotion` |
| `echo-muse-bridge` | A Linux host beside the Muse gadget service | Its own Muse SDK token | Submits tasks to Muse and receives results through one gadget command (section 7) |

**Trust boundary.** Wyoming is plaintext and unauthenticated. hassmic's firewall admits inbound TCP 16384–32767
from the LAN, which includes Wyoming's 16700, so anyone on the LAN who connects gets the microphone stream of a
run. Item `el-w1-wyoming-lockdown` restricts 16700 to the bridge's address (an upstream hassmic option, or a rule
in our install step), and a T5 check proves that a second host is refused.

## 3. The bridge runs the conversation and the tools

Owner decision (2026-11-15): MCP runs on the bridge, on the owner's side and never at OpenAI. Delegation is
`client` only, never `responses`.

```mermaid
sequenceDiagram
    participant H as hassmic (Echo)
    participant B as echo-bridge (PC)
    participant L as GPT-Live
    participant P as Port OS MCP
    participant S as echo-screen (Echo)
    H->>B: run-pipeline, audio-start, audio-chunk (wake word heard)
    B->>L: open or reuse the Live call (credential, instructions, client delegation)
    B->>L: microphone audio (WebRTC track)
    L-->>B: reply audio and events (data channel, sideband)
    B->>H: audio-start, audio-chunk, audio-stop (reply)
    B-->>S: UiState speaking
    L->>B: session.delegation.created (target client)
    loop agent turn
        B->>B: model call with the MCP tools as function tools
        B->>P: tools/call (Bearer; refreshed with the DPoP-bound refresh token)
        P-->>B: tool result
    end
    B->>L: session.commentary.append (delegation.id, result)
    L-->>B: spoken answer
    B->>H: reply audio
```

| `echo-bridge` | hassmic on the Echo |
|---|---|
| The GPT-Live credential; creating and closing Live calls; session config (model, voice, instructions, `client` delegation) | Wake word, the echo canceller, the mute latch: hardware authority stays here |
| WebRTC peer and the `oai-events` data channel; optional sideband for observability and the kill switch | Streams the microphone of a run over Wyoming; with `-D` it keeps streaming while the reply plays |
| Agent loop: `tools/list` → function schemas → each `function_call` executed as MCP `tools/call` → output returned; at most 8 tool calls and a deadline per task | Plays the reply audio the bridge sends; reports `played` |
| Port OS credential, tool allowlist, confirmation policy, idempotency ledger; single writer of MCP calls | Nothing of the program's state |

Consequences:

- **No credential reaches the Echo.** The GPT-Live credential and the Port OS grant stay on the bridge host. A
  stolen Echo yields a Wyoming endpoint and a screen pairing token, both revocable from the bridge.
- **No Codex runs in the product.** The agent loop is a small crate (`echo-agent-loop`). The drafts' Codex
  app-server backend and its ARMv7 build (EL-004, `el-x-codex-spike`) are dropped.
- **No code of ours runs as root on the Echo.** The screen client is an ordinary app; hassmic is upstream's.
- **One turn per wake word in v1.** With `-D` a run ends at the reply's `audio-stop`; the Live call stays open for
  `session_idle_seconds`, so the next wake word continues the same conversation.

### 3.1 Spikes (W1)

| Spike | Question | Decides |
|---|---|---|
| S1 Live transport | Which Rust WebRTC stack carries GPT-Live's audio track and `oai-events` data channel from the PC (`str0m`, `webrtc-rs`, or `libdatachannel` through FFI)? Do `session.delegation.created` and `session.commentary.append` flow on the data channel, or only on the sideband WebSocket? | `echo-live`'s dependency (ADR); whether the bridge attaches the sideband for delegation |
| S2 Wyoming barge-in | With `-D`, how does the bridge cut a reply hassmic is already playing when GPT-Live cancels its response: an early `audio-stop`, `pause-satellite`, or nothing yet? What does hassmic send when the wake word is heard while it speaks? Measured against `hassmic-host` | Whether we need an upstream hassmic change; the barge-in budget |
| S3 Screen client | A minimal WebView app as hassmic's home app showing the bridge's page: memory with hassmic running, coexistence with the light band and hassmic's face and words layers, touch focus, start on boot | `echo-screen`'s shape (ADR); the fallback is Kiosk Satellite pointed at the page |
| S4 Live tools | `gpt-live-1` lists function calling. Tools declared directly on the Live session could skip the delegated model for one-step requests | Measured only; nothing depends on it |

### 3.2 Backend profiles

1. **C: client execution on the bridge** (above). The shipping path for M4–M6.
2. **B: Muse** through the gadget SDK wrapper (section 7). Feature-gated, for M7.

Per install, exactly one profile executes tools (the RFC's single-writer invariant).

## 4. GPT-Live access: the credential and the token exchange

The operator sets up GPT-Live access (owner, 2026-11-15). The bridge is the only component that ever holds the
credential. The code follows OpenClaw's implementation, which signs in with the operator's ChatGPT subscription
the way Codex does and uses that sign-in for realtime voice. References are pinned to OpenClaw
`032227a96023233f4c66aa8d172e5823a857a43a` and Codex `ee344ca9458351a6ea102795fa73110ad3ede5dc` (read
2026-11-15). OpenClaw is MIT and Codex is Apache-2.0: code ported from either keeps its notice, and Codex's NOTICE
file comes with it.

### 4.1 Two credential modes

`echo-bridge` supports two modes, chosen in its config and recorded in every trace as `provider_auth_mode`
(never the secret itself):

| Mode | Credential | Live session | Agent model calls | Status |
|---|---|---|---|---|
| `chatgpt` (default, the OpenClaw path) | The operator's ChatGPT sign-in: OAuth access token plus the `chatgpt-account-id` | `POST https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas`, model `gpt-live-1-codex` | `https://chatgpt.com/backend-api/codex/responses` | What OpenClaw ships; preview interface (section 4.5) |
| `api-key` | An OpenAI Platform API key | `POST /v1/live/sessions`, model `gpt-live-1` (the documented GPT-Live path) | `https://api.openai.com/v1/responses` | Documented; billed per use |

Either way the bridge is the WebRTC peer and the credential never leaves the bridge host. OpenClaw does the same:
its SDP exchange runs on its gateway server, so the OAuth token never reaches the browser.

### 4.2 Signing in (the `chatgpt` mode)

OpenClaw runs its own sign-in with Codex's public OAuth client instead of sharing Codex CLI's token file. The
bridge does the same.

- **Client and endpoints.** Client id `app_EMoamEEZ73f0CkXaXp7hrann` (Codex's), issuer `https://auth.openai.com`,
  scopes `openid profile email offline_access`, PKCE S256 with a `state` value
  ([OpenClaw authorization](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-oauth-authorization.runtime.ts#L3-L48),
  [Codex login server](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/server.rs#L597-L616)).
- **Device-code sign-in, the bridge's default.** `POST /api/accounts/deviceauth/usercode`; the operator approves
  at `https://auth.openai.com/codex/device`; the bridge polls `/api/accounts/deviceauth/token`, which returns an
  `authorization_code` and `code_verifier`; those go to `POST /oauth/token` with redirect
  `https://auth.openai.com/deviceauth/callback`
  ([OpenClaw device code](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-device-code.ts#L25-L43)). It needs no browser on
  the bridge host.
- **Browser sign-in, as an alternative.** Authorization code with PKCE and a loopback redirect
  `http://localhost:1455/auth/callback`; the code goes to `POST https://auth.openai.com/oauth/token` as a form post
  ([OpenClaw token request](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-oauth-token.runtime.ts#L22-L23)).
- **Account id.** From the access token's claim `https://api.openai.com/auth` → `chatgpt_account_id`
  ([OpenClaw JWT](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/packages/ai/src/utils/oauth/openai-chatgpt-jwt.ts#L2-L31)).
- **Refresh.** Form post `grant_type=refresh_token`, `refresh_token`, `client_id` to `/oauth/token`
  ([OpenClaw refresh](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-oauth-token.runtime.ts#L305-L319)). Codex refreshes
  when the access token is within 5 minutes of expiry or the last refresh is older than 8 days
  ([Codex refresh policy](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/auth/manager.rs#L232-L241)); the bridge uses the same policy.
- **Importing an existing Codex sign-in** (`~/.codex/auth.json`: `tokens.access_token`, `refresh_token`,
  `account_id`, `id_token`, `last_refresh`) is allowed once, as OpenClaw does
  ([OpenClaw import](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/codex/src/migration/cli-credentials.ts#L209-L235),
  [Codex auth.json](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/auth/storage.rs#L53-L77)). After the import the bridge owns its own
  copy. It never refreshes Codex CLI's file in place, so two programs never rotate the same refresh token.
- **Storage.** The bridge keeps `{access, refresh, expires, account_id}` in its own file, readable only by the
  bridge's user, outside the repository, and wrapped in `Secret<T>` in memory.

### 4.3 Creating a Live call (the `chatgpt` mode)

OpenClaw's wire format for `gpt-live-1-codex`
([OpenClaw quicksilver wire](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-quicksilver-wire.ts#L405-L425),
[control channel](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-quicksilver-wire.ts#L227-L249),
[credential selection](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-auth.ts#L10-L45)):

- `POST https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas` with a JSON body
  `{sdp, session}`.
- Headers: `Authorization: Bearer <access token>`, `chatgpt-account-id`, `OpenAI-Alpha: quicksilver=v2`,
  `session-id`, `thread-id`, `x-session-id`, an `originator` and a User-Agent naming this project
  ([OpenClaw headers](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/packages/ai/src/providers/openai-chatgpt-request-headers.ts#L24-L30)).
- The response carries the SDP answer for the bridge's own WebRTC peer. The control channel is `wss://api.openai.com/v1/live/{callId}`.
- Codex CLI has the same route against the same backend
  ([Codex realtime call](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/codex-api/src/endpoint/realtime_call.rs#L130-L229)).

The public `gpt-live-1` model needs a Platform API key; the subscription path uses `gpt-live-1-codex`
([OpenClaw model check](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-quicksilver-wire.ts#L389-L392)). Ephemeral client secrets
(`/v1/realtime/client_secrets`) are minted only with a Platform API key, so they are no way to hand the
subscription credential to another device either
([OpenClaw provider factory](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-voice-provider-factory.ts#L400-L430)).

### 4.4 The Codex token exchange, and why it is not the default

Codex can also trade its sign-in for a Platform API key: `POST https://auth.openai.com/oauth/token` with
`grant_type=urn:ietf:params:oauth:grant-type:token-exchange`, `client_id`, `requested_token=openai-api-key`,
`subject_token=<id_token>` and `subject_token_type=urn:ietf:params:oauth:token-type:id_token`, then reads
`access_token` as the key ([Codex obtain_api_key](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/server.rs#L1036-L1068)). Codex calls it
only after browser sign-in and ignores a failure ([call site](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/server.rs#L446-L451));
device-code sign-in stores no key. OpenClaw does not use the exchange at all: it sends the ChatGPT access token
itself.

So the bridge's default is OpenClaw's direct path. The exchange stays a documented way to obtain a key for the
`api-key` mode, but nobody has shown that an exchanged key works for `/v1/live/sessions`. M2 tries it once and
records the result in `docs/program/baselines.md`.

### 4.5 Risks the operator accepts with the `chatgpt` mode

- **Terms.** Using Codex's OAuth client from another program rests on OpenClaw's own statement that it is
  supported outside Codex CLI. Nothing from OpenAI was found that says so.
- **Entitlement.** Which subscription tiers get `gpt-live-1-codex` is not documented.
- **Stability.** The `chatgpt.com/backend-api/codex` routes and `OpenAI-Alpha: quicksilver=v2` look like preview
  interfaces and may change without notice. Contract tests pin captured traces, so a change fails T4 loudly
  instead of drifting.

If any of these bites, the bridge switches to the `api-key` mode with no change on the Echo: the Echo never sees
either credential, only Wyoming audio.

## 5. Wake word

Owner decision (2026-11-15): the wake word is the one echo-dot-assist uses, the Pryon "Alexa" model from the
firmware's `libpryon`. It needs no model of our own and no training. `hassmic` loads it on the Echo; nothing of ours touches Pryon.

What follows from it:

- **Bench interference.** Stock Echos in the house also wake on "Alexa". Run T5 scenarios in a room without a
  stock Echo in earshot, and record the room in every report. The bench already measured that a natural TTS voice
  at full level wakes an Echo through the Ring speaker, and that quiet or robotic voices are rejected as false
  wakes (`docs/program/baselines.md`).
- **Firmware dependency.** The Pryon library and model are the firmware's, so a firmware build nobody has probed
  may move them. The probe (section 8.5) records their hashes; 7809's `libpryon.so` differs from the pinned 7080's
  (`docs/program/baselines.md`).
- **Other keywords exist.** The firmware also carries "Amazon", "Computer" and "Echo"; switching is a hassmic
  setting, so it costs no code (`docs/program/decisions.md`, open).
- **False-wake rate** is a T6 soak measurement, not a T5 pass condition.

## 6. The bridge: what it runs and where

- **What it is.** A single Rust binary, `echo-bridge`, in this workspace, sharing `echo-contracts`, so the Wyoming,
  screen and GPT-Live protocols are contracts checked by the same CI gates as every other crate.
- **What it does.** It connects out to hassmic's Wyoming port on the Echo (`run-satellite`), owns the GPT-Live
  call (section 4), runs the agent loop and the MCP clients, and serves the screen page and its event stream to
  `echo-screen` on the LAN (TLS with a certificate pinned at pairing, plus the pairing token). An operator CLI on
  the same binary handles the ChatGPT sign-in, the Port OS link, pairing the screen and the kill switch. A
  loopback-only `GET /healthz` reports each link's state.
- **Audio.** hassmic announces its formats in `audio-start` (rate, width, channels); the bridge resamples between
  them and the WebRTC track's Opus at 48 kHz. Every queue is bounded and drops the oldest frame under backpressure.
  The bridge counts received microphone frames per run, which is half of the mute proof (section 8.4).
- **Where it runs first: the operator's Windows PC on the LAN,** in WSL beside the CI-identical toolchain. The
  credentials are the operator's own, so they stay on the operator's machine; the Echo is on the same LAN, so
  Wyoming never crosses the internet; nothing is exposed to the internet. The cost is that the device only works
  while the PC is on, which is fine for development and the bench.
- **Where it can move later.** The same binary builds for a Raspberry Pi (always on; it can also host the Muse
  bridge). Moving off the PC means moving the credentials, so it is an owner decision (`docs/program/decisions.md`).
- **It talks to Port OS directly.** The bridge is the Port OS OAuth client and the only MCP client. Section 9 says
  what it can use today and what waits for portos-backend.
- **Crates** (declared in `lanes.toml`, section 10): `echo-contracts`, `echo-config`, `echo-wyoming`, `echo-audio`,
  `echo-live`, `echo-agent-api`, `echo-agent-loop`, `echo-agent-muse`, `echo-mcp-auth`, `echo-mcp-client`,
  `echo-screen-contract`, `echo-bridge`, `echo-lab`, `echo-spoof-mcp`, `echo-muse-bridge` and `xtask`. The screen
  page lives in `web/screen/`; the `echo-screen` app's shape waits for spike S3.

## 7. Muse: a wrapper around the gadget SDK

Owner decision (2026-11-13). The Muse gadget SDK turns a Linux box into a gadget Muse can call. It gives two
channels, and the wrapper uses both:

- **To Muse:** `musegadget send-user-msg --session-id <s> --wait <seconds>` sends a user-role message into Muse and
  returns Muse's next reply, or exits 3 on timeout (1–600 s).
- **From Muse:** a command spec in `/etc/musegadget/commands.d/` that Muse can call. The wrapper installs exactly
  one, `echo.report_result {task_id, status, summary}`. That is the correlation channel the RFC said was missing.

| Operation | Implementation |
|---|---|
| `submit(task)` | Create `task_id` and a deadline. Send a message carrying the task, the `task_id`, and the instruction to finish by calling `echo.report_result` with that id. Persist the task |
| `events(task)` | `Progress` from interim replies; `Completed` or `Failed` only from an `echo.report_result` call whose `task_id` matches a live task. Reject unknown or expired ids |
| `cancel(task)` | Mark cancelled, send a cancel message, ignore later results for that id |
| Timeout | No `report_result` by the deadline: `Failed {retryable: false}` with an honest voice message. Never infer success from chat text |

Rules: Muse acts on Port OS through its own connector, never with the bridge's credentials. The Muse bridge never installs
`system.run`, `file.write` or other host commands. `echo-bridge` talks to the Muse bridge over an
authenticated LAN connection. A `report_result` never counts as user consent. The first proof (M7) is a
harmless Port OS read that Muse performs and reports, with correlated ids end to end.

## 8. Testing as structure

### 8.1 Test tiers

Every change names its tier and its oracle. Every milestone names the tiers it requires.

| Tier | Runs where | Network | Command | Oracle | Required for |
|---|---|---|---|---|---|
| T0 static | Every PR | None | `make lint` | Shape lint, rule lint, fmt, clippy, deny, schema snapshots | Every PR |
| T1 unit and property | Every PR | None | `make test-unit` | Assertions, proptest, loom on the refresh and queue paths | Every PR |
| T2 contract | Every PR | None | `make test-contract` | JSON fixtures, captured GPT-Live traces, captured `hassmic-host` Wyoming exchanges | Every PR touching contracts |
| T3 simulated system | Every PR | Loopback only | `echo-lab run <scenario> --driver sim` | Trace assertions on one golden event sequence, fake clock | Every milestone |
| T4 cloud integration | Nightly or on label | Real GPT-Live; Port OS pilot with simulator devices; `hassmic-host` in place of the Echo | `echo-lab run <scenario> --driver cloud` | Trace, provider events, Port OS audit; no hardware | M2, M4, M5 |
| T5 hardware in the loop | Operator-started | Real | `echo-lab run <scenario> --driver device --bench ring` | Bridge trace and hassmic's log (adb) plus bench observations (Ring audio, STT, frames) | M1, M3, M5–M7 |
| T6 soak and acoustic | Scheduled | Real | `echo-lab soak …` | Percentiles, leak slope, false-wake rate | Release |

- Test tags map to nextest profiles (`hardware`, `cloud`). A T0 rule fails any test that opens a socket to a
  non-loopback address without the `cloud` tag.
- A change's acceptance names a scenario file and a tier. "Compiles" or "unit tests pass" is never acceptance for a
  change in behavior.
- A milestone passes only on a T5 report. T3 green is the precondition, never the completion.

### 8.2 One scenario file, many drivers

Each milestone and failure case is written **once**, as data, and run in simulation, against the cloud and on the
device. The drivers differ; the oracle does not. The `sim` and `cloud` drivers run the real hassmic code:
`hassmic-host`, echo-dot-assist's host build at the pinned commit, which reads file audio, takes `SIGUSR1` as a
wake word and speaks the same Wyoming as the Echo. CI builds it once and caches it (`el-w0-hassmic-host`).

```yaml
# tests/scenarios/m4-spoof-tool.yaml
id: m4-spoof-tool
milestone: M4
tiers: [T3, T4, T5]
setup:
  mcp: spoof            # spoof | portos-pilot
  backend: client       # client | muse
steps:
  - say: "Alexa, call the test tool and tell me its result"   # sim: fixture PCM + SIGUSR1; device: Ring talkback
  - expect_trace:
      - session.delegation.created
      - tools/call {name: test.echo, count: 1}
      - session.commentary.append
  - expect_speech_contains: ["test", "echo"]                  # sim: transcript; device: STT of the Ring mic
  - expect_screen: {state: speaking}                          # sim: UiState; device: screencap + Ring frame
budgets:
  first_audio_after_endpoint_ms_p95: 1500
negative:
  - inject: {mcp: http_500}
    expect_speech_contains: ["couldn't"]
    expect_trace_absent: [tools/call {count: 2}]
```

| Step | Sim driver | Cloud driver | Device driver |
|---|---|---|---|
| `say` | Fixture PCM into `hassmic-host`, `SIGUSR1` for the wake word | The same, against real GPT-Live | Play through the Ring speaker (section 8.4) |
| GPT-Live | A fake Live peer replaying captured traces | Real GPT-Live | Real GPT-Live |
| `expect_trace` | Bridge trace | Bridge trace plus sideband events | Bridge trace plus hassmic's log over `adb` |
| `expect_speech_contains` | Transcript event of the fake | GPT-Live transcript event | STT of the Ring recording, plus the transcript event |
| `expect_screen` | `UiState` snapshot from the screen server | `UiState` snapshot | `adb exec-out screencap` golden diff, plus a Ring frame for physical truth |
| `press` / `touch` | Signals and fake key events into `hassmic-host`; DOM events on the screen page | Not applicable | `adb shell input` for touch; key events into the input node found by name (section 8.6) |

The report schema of the execution plan (its section 0.2) stands, plus `tier`, `driver`, `scenario_digest`,
`hassmic_commit` and `bench_session_ids`. `placement` is always `bridge_pc` until the bridge moves.

### 8.3 Milestones

Milestones accumulate: the installed image still demonstrates every earlier one.

| Milestone | Demonstration | Tiers | Needs |
|---|---|---|---|
| **M0** repository harness | Every T0 gate runs in CI and has been seen to fail on a seeded violation. `echo-lab check --milestone M0` passes. Nothing else starts before M0 | T0 | Phase F (section 10) |
| **B0** bench on a stock Echo | The Ring plays "Alexa, what time is it?" to a stock Echo; the bench records the reply; STT finds a time; a Ring frame shows the light ring. Mouth, ear and STT are proven (`docs/program/baselines.md`); the eye is not yet | T5 (stock device) | The Port OS CLI (done, v0.1.27) |
| **M1** hassmic and the bridge | hassmic is installed on the probed firmware with `PROTO=wyoming` and `-D` (owner's go-ahead first); after a reboot the bridge connects over Wyoming; a wake word reaches the bridge as a run with microphone audio; the screen client shows "Echo Live ready"; the mute switch shows on the screen and the bridge receives zero frames while latched; 10 restarts of the bridge and of hassmic each recover without a reboot; a second LAN host is refused on 16700 | T3, T5 | W0, W1 |
| **M2** GPT-Live connects | The bridge creates and closes a Live call in the `chatgpt` mode; the screen shows "Connected"; an invalid or expired credential shows a relink error instead of pretending | T3, T4, T5 | Section 4 |
| **M3** full-duplex voice | "Alexa, what time is it?" is answered through the Echo speaker; talking over the reply cuts it within the S2 budget and the new question is answered; mute stops outbound audio (hassmic's log and the bridge's counter both show zero frames) | T3, T5 | M1, M2, S1, S2 |
| **M4** spoof MCP tool call | "Call the test tool and tell me its result": exactly one correlated `session.delegation.created` → `tools/call` → `session.commentary.append`; the answer is spoken; an MCP error gives an honest fallback | T3, T4, T5 | Agent loop, spoof MCP |
| **M5** Port OS link | The bridge starts the device-code grant; the Show displays the code; the phone approves `endpoint:read`; `tools/list` and one `endpoint_query` on the "Echo test light" simulator succeed; refresh needs a proof from the same key; a replayed refresh token is rejected; a bridge restart keeps the grant; revoke forces a relink. Until portos-backend's change lands, T4 runs the same scenario with an authorization code + PKCE token | T4, T5 | Section 9 |
| **M6** MCP drives an animation | "Show me a happy face": the spoof MCP's `ui.show_emotion` makes the screen play an animated face (frames, not a still), then clear; audio stays usable. Spoof MCP only: Port OS does not grow an equivalent | T3, T5 | M4, `echo-screen` |
| **M7** Muse delegation | "Ask Muse to run the test job": correlated `delegation_id → task_id → Muse session → result`, spoken; a timeout gives a safe voice response | T4, T5 | Muse bridge (section 7) |

Device-code access tokens live 24 h by default, so an ordinary M5 run never refreshes on its own. The scenario forces
at least two refreshes (the bridge's `--refresh-now` operator command) and one replay, which must revoke the grant.

### 8.4 The bench

A Ring pan-tilt indoor camera next to the Echo, driven by the Port OS CLI (`port`, v0.1.27 or later). `echo-lab`
shells out to `port --output json`; it never carries its own Ring peer.

| Need | CLI surface |
|---|---|
| Hold one session that talks and listens | `port session hold <ring> --camera --listen` |
| Mouth | `port message play-audio --lease <l> <file>`, or `--text "..."` for TTS |
| Mouth and ear in one step | `port message play-audio --lease <l> --listen <s> --output reply.wav` (JSON reports `playStartedAt`, `playEndedAt`, `recordStartedAt`, `recordEndedAt`) |
| Clock alignment | `port message play-audio --lease <l> --chirp --listen 3` at the start of each batch |
| Ear only | `port message record --lease <l> --audio-only` |
| Eye | held snapshots or the session's video track; PTZ to aim |
| Quiet hours | audio commands refuse before 09:00 local unless `--allow-quiet-hours` (never passed by `echo-lab`) |

Harness rules:

- **One session per batch.** Run scenarios for at most 11 minutes, close, wait 25 s, reopen. Ring closes a live
  view after about 12 minutes, refuses a new offer for about 20 s after a close, and locks out after too many
  sessions. Never one session per scenario.
- **Latency budgets are measured from the bridge and hassmic traces.** Ring audio arrives in bursts, so Ring-side
  timing is only good to about ±0.5 s. The Ring corroborates that sound happened.
- **Ear oracle:** STT on the Ring recording (a local Whisper model). Assert phrases, not exact transcripts. Record
  replies after the clip ends: the Ring microphone ducks while its own speaker plays. Keep recordings only for
  failed runs, outside git, and delete them after triage.
- **Eye oracle:** pixel-exact checks from `adb exec-out screencap -p` against goldens. The Ring frame answers only
  what screencap cannot: is the panel lit, does the light ring show the right pattern, is the mute LED red. OCR only
  for large text.
- **Privacy proof is instrumented.** The camera can see the mute LED; it cannot prove no frame left the device. That
  comes from hassmic's log of the run and the bridge's received-frame counter, both of which must show zero frames
  while the mute latch is set.

### 8.5 The firmware probe is generated

`echo-lab probe` runs the read-only probe (the six library hashes echo-dot-assist pins, `getevent -lp`,
`/proc/bus/input/devices`, `tinymix`, `dumpsys input`, the wake word models) over `adb`, without pushing a file,
and writes a manifest with `probe_timestamp` and `source`. The operator runs it once per firmware build; the
summary goes into `docs/program/baselines.md` (7809: 2026-11-15) and the raw output stays outside git, since it
holds the device serial. The `device` driver refuses an unprobed build.

### 8.6 Testing touch

hassmic owns the keys and the mute latch; Android owns touch, so the screen client sees ordinary touch events.

| Layer | Proves | How | Tier |
|---|---|---|---|
| 1. Hit testing | Page coordinates map to the right `UiAction` | The screen page at 1280 × 800 in a headless browser; synthetic pointer events at edges and corners; targets at least 48 dp; contrast checked in the snapshot test | T1/T3 |
| 2. Rendering | Every `UiState` renders without clipping | Snapshot of every state at 1280 × 800 in the headless browser | T1/T3 |
| 3. Injected touch on the device | Android → WebView → bridge → `UiAction` works on the Echo | `adb shell input tap` scaled to the panel; oracle: the bridge trace shows the `UiAction`, screencap shows the new state, a Ring frame confirms | T5 |
| 4. Glass | The digitizer that carries the glass, and its calibration | Once per firmware, a person taps five marked targets while `echo-lab calibrate` records raw `getevent`. 7809 has two touch devices (`fts_ts`, `amazon_touch`); this says which. Recorded on the Ring video | Manual, recorded |

## 9. Port OS dependencies

Owner decision (2026-11-15): the work is split by repository. portos-backend owns these changes; this repository
consumes them. The rule they follow: **`/mcp` accepts any valid Port OS access token for the MCP resource the same
way, whatever grant minted it** (authorization code + PKCE, device code, refresh). Nothing on `/mcp` is specific to
device login.

| ID | Change (portos-backend) | Status (2026-11-15) | Gates |
|---|---|---|---|
| PB-1 | `/mcp` accepts every valid access token for the MCP resource, independent of the grant that minted it | Merged (portos-monolith PR 2731); on the pilot since 2026-11-15 | M5 |
| PB-2 | The device authorization request takes `resource`, so a device-code token gets the MCP audience; discovery advertises `device_authorization_endpoint` | Merged (PR 2731); on the pilot since 2026-11-15 | M5 |
| PB-3 | A client that allows the `device_code` and `refresh_token` grants | Nothing to build: the built-in public CLI client `port1/systems/zero/oauth-clients/command-line-interface` already allows both. A dedicated `echo` client is optional, a management registration (public, auth method `none`) with no code change | M5 |
| PB-4 | DPoP proof checks (`typ`, `htu`, `htm`, `iat`, `jti` and the signature) at device authorization and at the token endpoint | Merged (portos-monolith PR 2735); on the pilot since 2026-11-15 | M5 hardening, not the first M5 run |
| PB-5 | Refresh rotation and revoke for device grants | Merged (PR 2731); on the pilot since 2026-11-15 | M5 revoke step |
| PB-6 | The "Echo test light" simulator, so M5 never touches a real lamp | Done: a LIGHT simulator on the owner's pilot account, visible through MCP. A dedicated test principal needs the owner's approval and does not exist | M5 |

What PR 2731 gives the bridge, once it is deployed:

- **Discovery.** `GET /.well-known/oauth-authorization-server` carries `device_authorization_endpoint`
  (`<base>/auth/device-authorization`) and `dpop_signing_alg_values_supported` (ES256, ES384, ES512, RS256, RS384,
  RS512, EdDSA). The bridge reads both from discovery and never hard-codes a path.
- **Device authorization.** `POST /auth/device-authorization` (form) with `resource` set to the `resource` of
  `/.well-known/oauth-protected-resource` (the server's `/mcp` URL). Any other value answers 400 `invalid_target`.
  Without `resource` the grant mints an old first-party token, which `/mcp` refuses with 401, so `echo-mcp-auth`
  always sends it and a T1 test asserts so. The request carries a DPoP proof: this is where the bridge's key is
  registered.
- **Token.** The `device_code` grant at `/auth/token`, with a DPoP proof from the same key, returns the same resource-bound pair as the browser code
  grant: an access token (`aud` = the resource, `token_use=access`, a `grant_id`, the scopes the owner granted;
  24 h by default) and a refresh token (30 days, `cnf.jkt` = the bridge's DPoP key thumbprint).
- **`/mcp` admission** is unchanged and grant-agnostic: `token_use=access`, the issuer, the MCP audience, a subject
  and an active `grant_id`. Access tokens are plain Bearer.
- **Refresh** is `grant_type=refresh_token` with a DPoP proof from the same key. Refresh tokens rotate; a replayed
  one revokes the whole grant, so the next `/mcp` call answers 401. The bridge must persist the rotated refresh
  token before it uses the new access token, or a crash between the two forces a relink.
- **Revoke** is `POST /auth/revoke` with `token=<refresh token>` and `client_id`; it ends MCP access.

PR 2731 is on the pilot (2026-11-15; `docs/program/baselines.md` has the live check). `echo-mcp-auth` is built and
tested against a mock authorization server that implements exactly this (T1–T3), and M5's device-code run on T4
uses the pilot. PB-4 is on the pilot too (PR 2735, 2026-11-15): a proof whose `htu` names another endpoint is
refused, so the T4 run proves on the real server that the bridge signs the exact URL. The `htu` is the exact
endpoint URL on the auth or api origin; the server ignores query strings and never trusts the `Host` header.

**DPoP is sent at exactly two places** (owner, 2026-11-15): device authorization (key registration) and the token
endpoint (`device_code` redemption and every refresh). `/mcp`, endpoint enumeration and every other API take the
plain Bearer access token, a signed JWT, with no proof; no `DPoP-Nonce` and no `ath` anywhere. Each proof has
`typ` `dpop+jwt`, `htm` `POST`, `htu` equal to the exact URL posted to (the discovery URL), a fresh random `jti`
and `iat` now; a proof for another path or host is refused with `invalid_dpop_proof`. `echo-mcp-auth` sends DPoP
on `/auth/device-authorization` and `/auth/token` only, and a T1 test asserts that no other request carries a
`DPoP` header. The drafts' resource-server proof checks, nonce retry and `ath` (`docs/design/`) are dropped.
Server-sent elicitation is deferred: Port OS MCP is stateless (POST only, no session ids, `GET /mcp` answers 405),
and no Port OS tool needs approval. Elicitation is tested only against the spoof MCP. Anything else this
repository needs from Port OS is asked of the portos-backend coordinator, never worked around.

## 10. Phase F: the repository harness, before any crate

Process and review trust CI alone, so the gates exist first. Each item's acceptance is: **the gate runs in CI, and
the pull request shows it failing on a seeded violation**, then removes the seed. A gate nobody has seen fail is not
evidence. There is no baseline file, so every rule is strict from the first commit. Phase F runs on the factory
once its smoke test has passed (`docs/program/coordinator.md`).

| Item | Delivers | Seeded-violation proof |
|---|---|---|
| `el-f1-readme-makefile` | README build section; `Makefile` with `lint`, `test-unit`, `test-contract`, `test-sim`, `build-host`, `ci`; `rust-toolchain.toml` pinned to 1.100.0; a Cargo workspace whose only member is `xtask`; `lanes.toml` listing exactly the workspace's crates, each with a `kind` (`lib`, `bin`, `ffi` or `tool`). A crate joins `lanes.toml` in the change that creates it | `make lint` exits non-zero on an unformatted file |
| `el-f2-rust-strict` | Workspace `[lints]`: `unsafe_code = "forbid"` except in crates `lanes.toml` marks `ffi`; clippy `pedantic` plus `unwrap_used`, `expect_used`, `panic`, `print_stdout`, `dbg_macro`, `todo`, `undocumented_unsafe_blocks`, `too_many_lines` (60), `cognitive_complexity` (15); `-D warnings`; rustfmt config | An `unwrap()`, an undocumented `unsafe` block and a 70-line function each fail |
| `el-f3-shape-lint` | `cargo xtask lint-shape`: the crate set equals `lanes.toml`; files at most 500 lines; at most 10 files per module directory, tests included; names carry their area prefix | An undeclared crate and a 501-line file both fail |
| `el-f4-layering` | `cargo xtask lint-deps` over `cargo metadata`: `echo-contracts`, `echo-config` and `echo-screen-contract` have no network, hardware or async-runtime deps; `echo-wyoming`, `echo-audio` and `echo-live` never depend on `agent-*` or `mcp-*`; `agent-*` reach each other only through `agent-api`; OAuth types only in `echo-mcp-auth`; plus `cargo-deny` | `echo-audio` depending on `echo-mcp-client` fails |
| `el-f6-rules-lint` | A new `Cargo.lock` package needs an ADR in `docs/adr/` that names it; gitleaks over the tree. The rules that need product types (schema snapshots, the `Secret<T>` compile-fail test, network test tags) land in W0 with `echo-contracts` and become part of `lint-rules` there | A dependency without an ADR fails; a committed fake key fails gitleaks |
| `el-f7-scripts-web-lint` | `shellcheck` and `shfmt` for `scripts/`; `prettier`, `eslint` and `tsc --noEmit` for `web/screen/` once it exists | A shellcheck warning fails |
| `el-f8-docs-lint` | markdownlint, lychee, Mermaid compile, frontmatter check, a docs index | A broken link fails |
| `el-f9-echo-lab-skeleton` | `echo-lab` with scenario and report JSON schemas, a scenario validator, the artifact layout; `echo-lab check --milestone M0` | An invalid scenario file fails validation |
| `el-f10-ci` | The required checks in section 11 as GitHub Actions jobs calling the make targets; branch protection on `main` requiring them | A red required check blocks merge; a seeded coverage drop fails `coverage` |

The drafts' `el-f5-lane-ownership` stays dropped, and so does the native lint item: no C of ours exists in this
repository. The drafts' `build-armv7` gate goes with the on-device daemon; `echo-screen`'s build becomes a gate in
W1 once spike S3 has chosen its shape.

Order: F1 first. F2–F4 and F6–F8 are independent after it (each owns a separate config or xtask file) and run as
parallel factory lanes. F9 and F10 come last. M0 passes when F10 has merged with every seeded proof linked.

## 11. CI gates

CI is the quality bar. Every gate below is a required status check on `main` from the day `el-f10` merges.
Nobody merges this repository with `--admin`: a red required check means the pull request waits.

Every job calls a make target, and the make target is what an agent runs locally. `make ci` runs the same list in
the same order. `xtask ci-parity` fails when `.github/workflows/ci.yml` and the `ci` target name different jobs.

**Required on every pull request:**

| Check | Make target | What fails it |
|---|---|---|
| `format` | `make fmt-check` | `cargo fmt --check`, `shfmt -d`, `prettier --check` |
| `lint-rust` | `make lint-rust` | clippy with the `el-f2` lint set and `-D warnings` |
| `lint-shape` | `make lint-shape` | Undeclared crate, file over 500 lines, module directory over 10 files |
| `lint-deps` | `make lint-deps` | A forbidden crate edge; `cargo-deny` licenses, advisories, duplicates, bans |
| `lint-rules` | `make lint-rules` | Schema drift, a dependency without an ADR, a secret-shaped string, `Debug` on a secret type, an untagged network test |
| `lint-scripts-web` | `make lint-scripts-web` | shellcheck, eslint, `tsc --noEmit` |
| `lint-docs` | `make lint-docs` | markdownlint, lychee, Mermaid compile, frontmatter |
| `test-unit` | `make test-unit` | Any T1 failure. nextest runs each test once with no retries: a test that passes on retry is a failure |
| `test-contract` | `make test-contract` | A T2 fixture, captured GPT-Live trace or captured Wyoming exchange that no longer round-trips |
| `test-sim` | `make test-sim` | Any scenario in `tests/scenarios/` failing under `--driver sim` against the cached `hassmic-host` (T3) |
| `test-concurrency` | `make test-concurrency` | loom over the refresh, audio queue and ledger paths; Miri over any `ffi` crate's tests |
| `coverage` | `make coverage` | A floor below missed (`cargo llvm-cov` over T1–T3) |

**Coverage floors** are fixed numbers, not a ratchet file:

| Scope | Line | Branch |
|---|---|---|
| Whole workspace | 80% | 70% |
| `echo-mcp-auth`, `echo-mcp-client`, `echo-agent-loop`, `echo-wyoming`, `echo-bridge` | 90% | 85% |
| `echo-contracts`, `echo-config`, `echo-screen-contract` | 95% | 90% |
| Lines a pull request changes | 85% | — |

Generated code is excluded through one list in `.config/coverage.toml`; adding to it needs an ADR.

**Time budget.** Required checks finish in under 10 minutes with a warm cache (the `hassmic-host` build is
cached by its pinned commit). Each T1 test has a 30 s timeout and each T3 scenario 120 s, on the fake clock. A
check that outgrows its budget becomes its own plan item; it is never made optional.

**Nightly and on label, never on the pull request path:**

| Job | Trigger | What fails it |
|---|---|---|
| `test-cloud` (T4) | Nightly, or the `cloud` label | A scenario under `--driver cloud` against GPT-Live and the Port OS pilot |
| `mutants` | Nightly | `cargo-mutants` score below 70% on `echo-mcp-auth` and `echo-agent-loop` |
| `deps-fresh` | Weekly | `cargo-deny` against the latest advisory database |
| `hassmic-upstream` | Weekly | `test-sim` against echo-dot-assist's latest `main`, so an upstream change shows before we move the pin |

T5 and T6 never run in CI. The milestone report directory and `docs/program/milestones/<id>.md` are their record.

## 12. Phase order after M0

The coordinator submits each phase to the factory as one idea batch with its `DEPENDS_ON` edges and a closing
loopback (`docs/program/coordinator.md`), and fans out inside a phase only where pieces are independent.

1. **W0 contracts and the host Echo.** `echo-contracts` (ids, the Wyoming events generated from captured
   `hassmic-host` exchanges, `LiveEvent` generated from captured GPT-Live traces, `TaskEvent`), `echo-screen-contract`
   (`UiState`, `UiAction`), the agent traits and fakes; `el-w0-hassmic-host` (echo-dot-assist at a pinned commit,
   built and cached in CI); `echo-lab probe`. Done when T2 fixtures round-trip and the schema snapshot is stable.
2. **W1 parallel pieces.** `echo-wyoming` (client against `hassmic-host`), `echo-audio`, `echo-live` (spike S1
   first), `echo-mcp-client` (Bearer) and `echo-spoof-mcp`, `echo-mcp-auth` against a mock authorization server,
   the screen page and `echo-screen` (spike S3 first), `el-w1-wyoming-lockdown`, the `echo-lab` sim driver, spike
   S2. Done when `m4-spoof-tool` passes with `--driver sim`.
3. **W2 integration.** The agent loop, `echo-bridge`, the golden trace. Done when `m4-spoof-tool` passes with
   `--driver cloud`, and M5's scenario with an authorization code token.
4. **B0 bench** runs in parallel from W0: the `echo-lab` device driver over the Port OS CLI, the STT oracle,
   `echo-lab calibrate`, and the Ring frame of the light ring.
5. **W3 device.** hassmic installed on the Echo (the owner's go-ahead), then M1, M3, M5 and M6 as operator-started
   T5 runs. PR 2731 and the PB-6 simulator are on the pilot.
6. **Muse** (`el-w3-muse-bridge`), gating M7.

Contract changes after W0 are their own pull request, made by the coordinator, never a side effect of a feature.
