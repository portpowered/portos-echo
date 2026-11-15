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

How the work is run (one coordinator, bounded `you` dispatches) is in `docs/program/coordinator.md`. The standing
rules are in `docs/program/rules.md`. Measured facts are in `docs/program/baselines.md`. Decisions and open
questions are in `docs/program/decisions.md`.

## 1. What we are building

A rooted Echo Show 8 (2nd gen, `athena`) runs our own voice stack instead of Alexa's:

1. The firmware's Pryon wake word engine hears "Alexa" (section 5).
2. The Echo opens a GPT-Live session over WebRTC. Audio flows Echo ↔ OpenAI directly.
3. When GPT-Live delegates a task, a small agent loop on the Echo plans it with a model that the gateway serves,
   and executes every MCP tool call itself: against Port OS MCP for the home, a spoof MCP for tests, and later
   other MCP servers.
4. The Show's screen, light ring, buttons and mute switch reflect what the system is doing.

The overall shape of the drafts stands: a C hardware shim, a Rust supervisor, WebRTC to GPT-Live, one delegated
agent that owns MCP, and milestones proven on the device. Five premises were wrong, and each changes the plan:

1. **A gateway is required in every topology.** GPT-Live creates a session from your server, with your credential:
   the client sends its SDP offer to your server, which creates the call. That holds for a Platform API key and for
   the ChatGPT sign-in OpenClaw uses (section 4), and neither credential belongs on a rooted device. So the drafts'
   "optional LAN gateway" is required. It handles the models; the Echo handles the tools (section 3).
2. **The drafts' GPT-Live event names are wrong.** Delegation arrives as `session.delegation.created` with
   `delegation.target` set to `client` or `responses`. Results go back with `session.commentary.append`. There is
   no `delegation.context.append`. Contracts are generated from captured traces, never from the RFC.
3. **Port OS MCP does not yet accept the device grant.** Device-code login and DPoP exist on Port OS for REST
   clients, but `/mcp` accepts only Bearer tokens from the authorization code + PKCE flow, and has no elicitation.
   Four Port OS backend changes are needed before milestone M5 (section 9).
4. **Muse is reached through a wrapper around the Muse gadget SDK,** with an explicit callback command for
   correlation. Muse never receives the Echo's credentials (section 7).
5. **The bench is a Ring camera driven by the Port OS CLI.** One held Ring session plays utterances into the room,
   records the reply and watches the screen. It works today on a stock Echo (`docs/program/baselines.md`).

## 2. Architecture

```text
Echo Show (Rust + C shim)                   Gateway (Rust, on the LAN)           OpenAI
  wake word (Pryon "Alexa")                   holds the GPT-Live credential      GPT-Live session
  audio capture/playback, screen, keys  --->  relays the SDP offer          ---> model calls for the
  WebRTC peer to GPT-Live  <== audio ==>      proxies the agent's model calls     delegated agent
  agent loop + MCP client               ---------------------------------------> Port OS MCP, other MCP
```

| Component | Runs on | Holds | Does |
|---|---|---|---|
| `echo-daemon` and its crates | The Echo (armv7, Fire OS 7) | Port OS credential (device key, DPoP-bound refresh token, Bearer access token); the gateway signing key | Wake word, audio, screen, buttons; WebRTC peer and the `oai-events` data channel; the agent loop; every MCP call; `session.commentary.append` |
| `native/` C shim | The Echo | Nothing secret | Board access the firmware forces into C: Pryon, ALSA mixer paths, evdev, LEDs, framebuffer |
| `echo-gateway` | The operator's LAN host first (section 6) | The GPT-Live credential; the pinned public key of each paired Echo | Creates Live sessions; relays the SDP; serves the delegated agent's model calls with the Echo's tools as function tools; caps, audit log, kill switch |
| `echo-lab` | The operator's workstation and CI | Bench session leases (never committed) | Runs scenario files with the `sim`, `cloud` or `device` driver and writes reports |
| `echo-muse-bridge` | A Linux host beside the Muse gadget service | Its own Muse SDK token | Submits tasks to Muse and receives results through one gadget command (section 7) |

## 3. MCP runs on the Echo, models run on the gateway

Owner decision (2026-11-13): every MCP invocation executes on the Echo, and the server side handles the models.

```mermaid
sequenceDiagram
    participant E as Echo (MCP client + agent loop)
    participant G as Gateway (model side)
    participant L as GPT-Live
    participant P as Port OS MCP
    E->>G: SDP offer (signed with the Echo's device key)
    G->>L: create the Live call (credential, instructions, client delegation; section 4)
    L-->>G: SDP answer
    G-->>E: SDP answer
    E->>L: WebRTC audio + oai-events data channel (both directions)
    L->>E: session.delegation.created (target client)
    loop agent turn
        E->>G: model request (task, tool schemas from tools/list)
        G->>G: model call with function tools, no server-side MCP
        G-->>E: function_call(tool, arguments)
        E->>P: tools/call (Bearer, refreshed with the DPoP-bound refresh token)
        P-->>E: tool result
        E->>G: function output
    end
    G-->>E: final answer text
    E->>L: session.commentary.append (delegation.id, result)
    L-->>E: spoken answer
```

| Gateway (server side) | Echo (client side) |
|---|---|
| The GPT-Live credential; creating Live sessions; session config (model, voice, instructions, `client` delegation) | WebRTC peer and the `oai-events` data channel; receives `session.delegation.created` |
| Model proxy for the delegated agent: the Echo's tool list passed as **function** tools | Agent loop: `tools/list` → function schemas → each `function_call` executed as MCP `tools/call` → output returned |
| Model choice, prompts, token and cost caps, audit log of model requests | Port OS credential; tool allowlist and confirmation policy |
| Optional sideband attach for observability and a kill switch | Single writer of MCP calls, idempotency ledger, `session.commentary.append` |

Consequences:

- **The Port OS token never leaves the Echo.** The gateway never holds it; it holds only the GPT-Live credential.
- **No Codex runs on either side.** The agent loop is a small crate (`echo-agent-loop`): tool discovery, a bounded
  turn loop (at most 8 tool calls and a deadline per task), and policy. The drafts' on-device Codex build (EL-004)
  becomes `el-x-codex-spike`, run after M4, gating nothing.
- **The gateway authenticates the Echo,** or it becomes an open model proxy. The Echo's gateway public key is
  pinned at pairing and every gateway request is signed with it. It is a separate key from the DPoP key.
- **Spike first (W1):** confirm that `session.delegation.created` and `session.commentary.append` flow over the
  WebRTC data channel, not only over the sideband WebSocket. If they do not, the gateway attaches the sideband and
  relays delegation events to the Echo over its authenticated connection. Tools still run on the Echo.
- **Second spike:** `gpt-live-1` lists function calling. Tools declared directly on the Live session could skip the
  delegated model for one-step requests. Measure it; do not depend on it.

Backend profiles:

1. **C: client execution** (above). The shipping path for M4–M6.
2. **B: Muse** through the gadget SDK wrapper (section 7). Feature-gated, for M7.
3. **X: Codex app-server.** An optional experiment. No milestone depends on it.

Per install, exactly one profile executes tools (the RFC's single-writer invariant).

## 4. GPT-Live access: the credential and the token exchange

The operator sets up GPT-Live access (owner, 2026-11-15). The gateway is the only component that ever holds the
credential. The code follows OpenClaw's implementation, which signs in with the operator's ChatGPT subscription
the way Codex does and uses that sign-in for realtime voice. References are pinned to OpenClaw
`032227a96023233f4c66aa8d172e5823a857a43a` and Codex `ee344ca9458351a6ea102795fa73110ad3ede5dc` (read
2026-11-15). OpenClaw is MIT and Codex is Apache-2.0: code ported from either keeps its notice, and Codex's NOTICE
file comes with it.

### 4.1 Two credential modes

`echo-gateway` supports two modes, chosen in its config and recorded in every trace as `provider_auth_mode`
(never the secret itself):

| Mode | Credential | Live session | Agent model calls | Status |
|---|---|---|---|---|
| `chatgpt` (default, the OpenClaw path) | The operator's ChatGPT sign-in: OAuth access token plus the `chatgpt-account-id` | `POST https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas`, model `gpt-live-1-codex` | `https://chatgpt.com/backend-api/codex/responses` | What OpenClaw ships; preview interface (section 4.5) |
| `api-key` | An OpenAI Platform API key | `POST /v1/live/sessions`, model `gpt-live-1` (the documented GPT-Live path) | `https://api.openai.com/v1/responses` | Documented; billed per use |

Either way the gateway relays the SDP and the credential never leaves the gateway. OpenClaw does the same: its SDP
exchange runs on its gateway server, so the OAuth token never reaches the browser.

### 4.2 Signing in (the `chatgpt` mode)

OpenClaw runs its own sign-in with Codex's public OAuth client instead of sharing Codex CLI's token file. The
gateway does the same.

- **Client and endpoints.** Client id `app_EMoamEEZ73f0CkXaXp7hrann` (Codex's), issuer `https://auth.openai.com`,
  scopes `openid profile email offline_access`, PKCE S256 with a `state` value
  ([OpenClaw authorization](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-oauth-authorization.runtime.ts#L3-L48),
  [Codex login server](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/server.rs#L597-L616)).
- **Device-code sign-in, the gateway's default.** `POST /api/accounts/deviceauth/usercode`; the operator approves
  at `https://auth.openai.com/codex/device`; the gateway polls `/api/accounts/deviceauth/token`, which returns an
  `authorization_code` and `code_verifier`; those go to `POST /oauth/token` with redirect
  `https://auth.openai.com/deviceauth/callback`
  ([OpenClaw device code](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-device-code.ts#L25-L43)). It needs no browser on
  the gateway host.
- **Browser sign-in, as an alternative.** Authorization code with PKCE and a loopback redirect
  `http://localhost:1455/auth/callback`; the code goes to `POST https://auth.openai.com/oauth/token` as a form post
  ([OpenClaw token request](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-oauth-token.runtime.ts#L22-L23)).
- **Account id.** From the access token's claim `https://api.openai.com/auth` → `chatgpt_account_id`
  ([OpenClaw JWT](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/packages/ai/src/utils/oauth/openai-chatgpt-jwt.ts#L2-L31)).
- **Refresh.** Form post `grant_type=refresh_token`, `refresh_token`, `client_id` to `/oauth/token`
  ([OpenClaw refresh](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/openai-chatgpt-oauth-token.runtime.ts#L305-L319)). Codex refreshes
  when the access token is within 5 minutes of expiry or the last refresh is older than 8 days
  ([Codex refresh policy](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/auth/manager.rs#L232-L241)); the gateway uses the same policy.
- **Importing an existing Codex sign-in** (`~/.codex/auth.json`: `tokens.access_token`, `refresh_token`,
  `account_id`, `id_token`, `last_refresh`) is allowed once, as OpenClaw does
  ([OpenClaw import](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/codex/src/migration/cli-credentials.ts#L209-L235),
  [Codex auth.json](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/auth/storage.rs#L53-L77)). After the import the gateway owns its own
  copy. It never refreshes Codex CLI's file in place, so two programs never rotate the same refresh token.
- **Storage.** The gateway keeps `{access, refresh, expires, account_id}` in its own file, readable only by the
  gateway's user, outside the repository, and wrapped in `Secret<T>` in memory.

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
- The response carries the SDP answer for the Echo. The control channel is `wss://api.openai.com/v1/live/{callId}`.
- Codex CLI has the same route against the same backend
  ([Codex realtime call](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/codex-api/src/endpoint/realtime_call.rs#L130-L229)).

The public `gpt-live-1` model needs a Platform API key; the subscription path uses `gpt-live-1-codex`
([OpenClaw model check](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-quicksilver-wire.ts#L389-L392)). Ephemeral client secrets
(`/v1/realtime/client_secrets`) are minted only with a Platform API key, so they are no way to put the
subscription credential on the Echo either
([OpenClaw provider factory](https://github.com/openclaw/openclaw/blob/032227a96023233f4c66aa8d172e5823a857a43a/extensions/openai/realtime-voice-provider-factory.ts#L400-L430)).

### 4.4 The Codex token exchange, and why it is not the default

Codex can also trade its sign-in for a Platform API key: `POST https://auth.openai.com/oauth/token` with
`grant_type=urn:ietf:params:oauth:grant-type:token-exchange`, `client_id`, `requested_token=openai-api-key`,
`subject_token=<id_token>` and `subject_token_type=urn:ietf:params:oauth:token-type:id_token`, then reads
`access_token` as the key ([Codex obtain_api_key](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/server.rs#L1036-L1068)). Codex calls it
only after browser sign-in and ignores a failure ([call site](https://github.com/openai/codex/blob/ee344ca9458351a6ea102795fa73110ad3ede5dc/codex-rs/login/src/server.rs#L446-L451));
device-code sign-in stores no key. OpenClaw does not use the exchange at all: it sends the ChatGPT access token
itself.

So the gateway's default is OpenClaw's direct path. The exchange stays a documented way to obtain a key for the
`api-key` mode, but nobody has shown that an exchanged key works for `/v1/live/sessions`. M2 tries it once and
records the result in `docs/program/baselines.md`.

### 4.5 Risks the operator accepts with the `chatgpt` mode

- **Terms.** Using Codex's OAuth client from another program rests on OpenClaw's own statement that it is
  supported outside Codex CLI. Nothing from OpenAI was found that says so.
- **Entitlement.** Which subscription tiers get `gpt-live-1-codex` is not documented.
- **Stability.** The `chatgpt.com/backend-api/codex` routes and `OpenAI-Alpha: quicksilver=v2` look like preview
  interfaces and may change without notice. Contract tests pin captured traces, so a change fails T4 loudly
  instead of drifting.

If any of these bites, the gateway switches to the `api-key` mode with no change on the Echo: the Echo only ever
sees an SDP answer and the gateway's agent endpoint.

## 5. Wake word

Owner decision (2026-11-15): the wake word is the one echo-dot-assist uses, the Pryon "Alexa" model from the
firmware's `libpryon`. It is the only keyword the firmware carries, so it needs no model of our own and no
training. `native/` loads it the way echo-dot-assist's `hassmic` does (MIT; keep its notice on any code taken).

What follows from it:

- **Bench interference.** Stock Echos in the house also wake on "Alexa". Run T5 scenarios in a room without a
  stock Echo in earshot, and record the room in every report. The bench already measured that a natural TTS voice
  at full level wakes an Echo through the Ring speaker, and that quiet or robotic voices are rejected as false
  wakes (`docs/program/baselines.md`).
- **Firmware dependency.** The Pryon library and model are the firmware's, so a firmware build nobody has probed
  may move them. The probe (section 8.5, `el-w0-board-manifest`) records their paths and hashes.
- **False-wake rate** is a T6 soak measurement, not a T5 pass condition.

## 6. The gateway: what it runs and where

The drafts left the gateway undefined. Recommendation (to be confirmed, `docs/program/decisions.md`):

- **What it is.** A single Rust binary, `echo-gateway`, in this workspace. It shares `echo-contracts` with the
  Echo, so the gateway protocol is a contract checked on both sides, and it gets the same CI gates as every other
  crate. HTTP server (`axum` on `tokio`), TLS with a self-signed certificate that the Echo pins at pairing.
- **What it does.** Four endpoints, all requiring a request signed by a paired Echo's key: `POST /pair` (operator
  approves on the gateway host), `POST /live/sessions` (relays the SDP offer, returns the answer), `POST /agent/turn`
  (one delegated-agent model call with the Echo's tools as function tools), and `POST /live/sessions/{id}/close`.
  Plus a loopback-only `GET /healthz` and an operator kill switch.
- **Where it runs first: the operator's Windows PC on the LAN.** Reasons: the credential is the operator's own
  ChatGPT sign-in (section 4), so it stays on the operator's own machine; the Echo is on the same LAN, so the SDP
  relay adds no internet hop; nothing is exposed to the internet. The cost is that the gateway is only up while the
  PC is on. That is fine for development and the bench.
- **Where it can move later.** The same binary cross-builds for a Raspberry Pi (always on; it can also host the
  Muse bridge) or a small cloud VM. Moving off the PC means moving the credential, so it is an owner decision, not
  a coordinator one.
- **Not a dependency on Port OS.** The gateway never talks to Port OS. Port OS is reached only by the Echo.

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

Rules: Muse acts on Port OS through its own connector, never with Echo credentials. The bridge never installs
`system.run`, `file.write` or other host commands. The Echo talks to the bridge over an authenticated LAN
connection, signed like the gateway's. A `report_result` never counts as user consent. The first proof (M7) is a
harmless Port OS read that Muse performs and reports, with correlated ids end to end.

## 8. Testing as structure

### 8.1 Test tiers

Every change names its tier and its oracle. Every milestone names the tiers it requires.

| Tier | Runs where | Network | Command | Oracle | Required for |
|---|---|---|---|---|---|
| T0 static | Every PR | None | `make lint` | Shape lint, rule lint, fmt, clippy, deny, schema snapshots | Every PR |
| T1 unit and property | Every PR | None | `make test-unit` | Assertions, proptest, loom on the refresh and queue paths | Every PR |
| T2 contract | Every PR | None | `make test-contract` | JSON fixtures, captured GPT-Live traces, C ABI size and alignment on host and armv7 | Every PR touching contracts or FFI |
| T3 simulated system | Every PR | Loopback only | `echo-lab run <scenario> --driver sim` | Trace assertions on one golden event sequence, fake clock | Every milestone |
| T4 cloud integration | Nightly or on label | Real GPT-Live; Port OS pilot with simulator devices | `echo-lab run <scenario> --driver cloud` | Trace, provider events, Port OS audit; no hardware | M2, M4, M5 |
| T5 hardware in the loop | Operator-started | Real | `echo-lab run <scenario> --driver device --bench ring` | Device trace (adb) plus bench observations (Ring audio, STT, frames) | M1, M3, M5–M7 |
| T6 soak and acoustic | Scheduled | Real | `echo-lab soak …` | Percentiles, leak slope, false-wake rate | Release |

- Test tags map to nextest profiles (`hardware`, `cloud`). A T0 rule fails any test that opens a socket to a
  non-loopback address without the `cloud` tag.
- A change's acceptance names a scenario file and a tier. "Compiles" or "unit tests pass" is never acceptance for a
  change in behavior.
- A milestone passes only on a T5 report. T3 green is the precondition, never the completion.

### 8.2 One scenario file, many drivers

Each milestone and failure case is written **once**, as data, and run in simulation, against the cloud and on the
device. The drivers differ; the oracle does not.

```yaml
# tests/scenarios/m4-spoof-tool.yaml
id: m4-spoof-tool
milestone: M4
tiers: [T3, T4, T5]
setup:
  mcp: spoof            # spoof | portos-pilot
  backend: client       # client | muse
steps:
  - say: "Alexa, call the test tool and tell me its result"   # sim: inject PCM; device: Ring talkback
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
| `say` | Inject fixture PCM into `SimBoard` | Push PCM into a host WebRTC peer | Play through the Ring speaker (section 8.4) |
| `expect_trace` | In-process trace | Gateway trace plus sideband events | Gateway trace plus `adb` daemon trace |
| `expect_speech_contains` | GPT-Live transcript event | GPT-Live transcript event | STT of the Ring recording, plus the transcript event |
| `expect_screen` | `UiState` snapshot | `UiState` snapshot | `adb exec-out screencap` golden diff, plus a Ring frame for physical truth |
| `press` / `touch` | Fake evdev event | Not applicable | Events written into the real input node (section 8.6) |

The report schema of the execution plan (its section 0.2) stands, plus `tier`, `driver`, `scenario_digest` and
`bench_session_ids`. `placement` is recorded; after this plan `lan_gateway` is the expected value for M2, M4 and
M5, and it is not a fallback.

### 8.3 Milestones

Milestones accumulate: the installed image still demonstrates every earlier one.

| Milestone | Demonstration | Tiers | Needs |
|---|---|---|---|
| **M0** repository harness | Every T0 gate runs in CI and has been seen to fail on a seeded violation. `echo-lab check --milestone M0` passes. Nothing else starts before M0 | T0 | Phase F (section 10) |
| **B0** bench on a stock Echo | The Ring plays "Alexa, what time is it?" to a stock Echo; the bench records the reply; STT finds a time; a Ring frame shows the light ring. Mouth, ear and STT are proven (`docs/program/baselines.md`); the eye is not yet | T5 (stock device) | The Port OS CLI (done, v0.1.27) |
| **M1** app runs on the device | After a reboot the daemon starts, the screen shows "Echo Live ready", buttons work, the mute switch is reflected; restartable without a reboot (10 cycles) | T3, T5 | W0, W1 device lanes |
| **M2** GPT-Live connects | From the Echo, through the gateway, a Live session is created and closed; the screen shows "Connected"; an invalid credential shows a relink error instead of pretending | T3, T4, T5 | Gateway, section 4 |
| **M3** full-duplex voice | "Alexa, what time is it?" is answered through the Echo speaker; barge-in works while it speaks; mute stops outbound audio (counters prove it) | T3, T5 | M1, M2 |
| **M4** spoof MCP tool call | "Call the test tool and tell me its result": exactly one correlated `session.delegation.created` → `tools/call` → `session.commentary.append`; the answer is spoken; an MCP error gives an honest fallback | T3, T4, T5 | Agent loop, spoof MCP |
| **M5** Port OS device grant | The Show displays a device code; the phone approves `endpoint:read`; `tools/list` and one `endpoint_query` on the "Echo test light" simulator succeed; refresh needs a proof from the same key; a replayed refresh token is rejected and revokes the family; restart keeps the grant; revoke forces a relink | T4, T5 | Port OS changes (section 9) |
| **M6** MCP drives an animation | "Show me a happy face": the spoof MCP's `ui.show_emotion` plays an animated face (frames, not a still), then clears; audio stays usable. Spoof MCP only: Port OS does not grow an equivalent | T3, T5 | M4, UI lanes |
| **M7** Muse delegation | "Ask Muse to run the test job": correlated `delegation_id → task_id → Muse session → result`, spoken; a timeout gives a safe voice response | T4, T5 | Muse bridge (section 7) |

Access tokens on the Port OS pilot live 300 s, so a 30-minute M5 run refreshes about six times. Every refresh is a
DPoP proof, which makes the run a free soak of the bound refresh path.

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
- **Latency budgets are measured from the device and gateway traces.** Ring audio arrives in bursts, so Ring-side
  timing is only good to about ±0.5 s. The Ring corroborates that sound happened.
- **Ear oracle:** STT on the Ring recording (a local Whisper model). Assert phrases, not exact transcripts. Record
  replies after the clip ends: the Ring microphone ducks while its own speaker plays. Keep recordings only for
  failed runs, outside git, and delete them after triage.
- **Eye oracle:** pixel-exact checks from `adb exec-out screencap -p` against goldens. The Ring frame answers only
  what screencap cannot: is the panel lit, does the light ring show the right pattern, is the mute LED red. OCR only
  for large text.
- **Privacy proof is instrumented.** The camera can see the mute LED; it cannot prove no frame left the device. That
  comes from the daemon's outbound frame counters and the gateway's received-frame counters.

### 8.5 The board manifest is generated

`el-w0-board-manifest` writes a probe script (`getevent -il`, `getevent -lp`, `/proc/bus/input/devices`, `tinymix`,
`dumpsys input`, Pryon library paths and hashes). The operator runs it once per firmware build. The manifest records
`probe_timestamp` and `source`, and is never hand-written. A hardware adapter does not run on an unprobed build.

### 8.6 Testing touch

| Layer | Proves | How | Tier |
|---|---|---|---|
| 1. Hit testing | Panel coordinates map to the right `UiAction` | Pure function: raw `ABS_MT_POSITION_X/Y` → rotation and calibration from the manifest → screen point → target. Unit and property tests, including edges, corners and multi-touch slots | T1 |
| 2. Rendering | Targets exist, are big enough and aren't clipped | Desktop snapshots at 1280×800; lint target size (at least 48 dp) and contrast in the snapshot test | T1/T3 |
| 3. Injected touch on the device | Kernel input node → daemon → `UiAction` → screen works on the device | As root, write a touch sequence into the touch panel's evdev node (found by name, never `eventN`), scaled from the manifest's ranges. Oracle: daemon trace shows the `UiAction`, screencap shows the new state, a Ring frame confirms | T5 |
| 4. Glass | Digitizer, rotation and calibration match the manifest | Once per firmware or board revision, a person taps five marked targets while `echo-lab calibrate` records raw `getevent`. Recorded on the Ring video | Manual, recorded |

Touch needs no robot. Layers 1–3 are automated; layer 4 keeps layer 3 honest.

## 9. Port OS dependencies

Port OS (the smart-home MCP server) needs four changes before M5 can pass. They are Port OS work, tracked in the
Port OS repository, and they follow the pattern its Android TV client already uses (a built-in public client,
device code, DPoP-bound refresh token).

| ID | Change | Gates |
|---|---|---|
| PO-1 | The device grant can bind to the MCP resource: `resource` on device authorization; the access token gets the MCP audience, `token_use=access`, the approved scope and a grant id; discovery advertises `device_authorization_endpoint` | M5 |
| PO-2 | A built-in `echo-live` public client with the `device_code` and `refresh_token` grants, auth method `none` | M5 |
| PO-3 | DPoP checks on the token endpoint hardened (`htu`/`htm` matched, the registration proof's signature verified), `dpop_signing_alg_values_supported` advertised, and the device-auth guide corrected: tokens are `Bearer` | M5 |
| PO-5 | Rotation with replay-family revocation for device refresh tokens; `/auth/revoke` works for them; a device revoke route; refresh checks revoked status | M5 revoke step |

DPoP binds the refresh token only (owner, 2026-11-13); MCP requests use Bearer, with no DPoP nonce or `ath`.
Server-sent elicitation is deferred: Port OS MCP is stateless (POST only, no session ids, `GET /mcp` answers 405),
and no Port OS tool needs approval. Elicitation is tested only against the spoof MCP.

Until PO-1/2/3/5 merge, `echo-mcp-auth` and `echo-mcp-client` develop against the spoof MCP and a mock authorization
server. No interim auth flow is planned.

The Port OS test account gets a simulator light, "Echo test light", so M5 never touches a real lamp.

## 10. Phase F: the repository harness, before any crate

Process and review trust CI alone, so the gates exist first. Each item's acceptance is: **the gate runs in CI, and
the pull request shows it failing on a seeded violation**, then removes the seed. A gate nobody has seen fail is not
evidence. There is no baseline file, so every rule is strict from the first commit.

The coordinator builds these one at a time or dispatches independent ones in parallel (`docs/program/coordinator.md`).
The `el-` names are item names, not owners.

| Item | Delivers | Seeded-violation proof |
|---|---|---|
| `el-f1-readme-makefile` | README build section; `Makefile` with `lint`, `test-unit`, `test-contract`, `test-sim`, `build-host`, `build-armv7`, `ci`; `rust-toolchain.toml` and NDK pins; empty Cargo workspace with `lanes.toml` (the declared crate list) | `make lint` exits non-zero on an unformatted file |
| `el-f2-rust-strict` | Workspace `[lints]`: `unsafe_code = "forbid"` except in FFI crates; clippy `pedantic` plus `unwrap_used`, `expect_used`, `panic`, `print_stdout`, `dbg_macro`, `todo`, `undocumented_unsafe_blocks`, `too_many_lines` (60), `cognitive_complexity` (15); `-D warnings`; rustfmt config | An `unwrap()`, an undocumented `unsafe` block and a 70-line function each fail |
| `el-f3-shape-lint` | `cargo xtask lint-shape`: the crate set equals `lanes.toml`; files at most 500 lines; at most 10 files per module directory, tests included; names carry their area prefix | An undeclared crate and a 501-line file both fail |
| `el-f4-layering` | `cargo xtask lint-deps` over `cargo metadata`: `echo-contracts` and `echo-config` have no network, hardware or async-runtime deps; `device`, `audio` and `webrtc` never depend on `agent-*`, `mcp-*` or `live`; `agent-*` reach each other only through `agent-api`; OAuth types only in `echo-mcp-auth`; `echo-gateway` never depends on `echo-mcp-*`; plus `cargo-deny` | `echo-audio` depending on `echo-mcp-client` fails |
| `el-f6-rules-lint` | Schema snapshots must match; a new `Cargo.lock` crate needs an ADR named in the PR; gitleaks; `Secret<T>` never implements `Debug` or `Display` (compile-fail test); network test tags enforced (section 8.1) | A dependency without an ADR fails; `#[derive(Debug)]` on a token type fails |
| `el-f7-native-lint` | `clang-format`, `clang-tidy`, `cppcheck`, `-Wall -Wextra -Werror` for `native/`; ASan/UBSan host build; `shellcheck`/`shfmt` | A C warning fails the build |
| `el-f8-docs-lint` | markdownlint, lychee, Mermaid compile, frontmatter check, a docs index | A broken link fails |
| `el-f9-echo-lab-skeleton` | `echo-lab` with scenario and report JSON schemas, a scenario validator, the artifact layout; `echo-lab check --milestone M0` | An invalid scenario file fails validation |
| `el-f10-ci` | The required checks in section 11 as GitHub Actions jobs calling the make targets; branch protection on `main` requiring them | A red required check blocks merge; a seeded coverage drop fails `coverage` |

The drafts' `el-f5-lane-ownership` (one lane per PR, enforced) is dropped: with one coordinator there are no lane
owners to keep apart. `lanes.toml` stays as the declared crate list that `el-f3` checks.

Order: F1 first. F2–F4 and F6–F8 are independent after it (each owns a separate config or xtask file) and can be
dispatched together. F9 and F10 come last. M0 passes when F10 has merged with every seeded proof linked.

## 11. CI gates

CI is the quality bar. Every gate below is a required status check on `main` from the day `el-f10` merges.
Nobody merges this repository with `--admin`: a red required check means the pull request waits.

Every job calls a make target, and the make target is what an agent runs locally. `make ci` runs the same list in
the same order. `xtask ci-parity` fails when `.github/workflows/ci.yml` and the `ci` target name different jobs.

**Required on every pull request:**

| Check | Make target | What fails it |
|---|---|---|
| `format` | `make fmt-check` | `cargo fmt --check`, `clang-format --dry-run -Werror`, `shfmt -d` |
| `lint-rust` | `make lint-rust` | clippy with the `el-f2` lint set and `-D warnings` |
| `lint-shape` | `make lint-shape` | Undeclared crate, file over 500 lines, module directory over 10 files |
| `lint-deps` | `make lint-deps` | A forbidden crate edge; `cargo-deny` licenses, advisories, duplicates, bans |
| `lint-rules` | `make lint-rules` | Schema drift, a dependency without an ADR, a secret-shaped string, `Debug` on a secret type, an untagged network test |
| `lint-native` | `make lint-native` | clang-tidy, cppcheck, `-Wall -Wextra -Werror`, shellcheck |
| `lint-docs` | `make lint-docs` | markdownlint, lychee, Mermaid compile, frontmatter |
| `test-unit` | `make test-unit` | Any T1 failure. nextest runs each test once with no retries: a test that passes on retry is a failure |
| `test-contract` | `make test-contract` | A T2 fixture or captured GPT-Live trace that no longer round-trips; C ABI size or alignment changes on host or armv7 |
| `test-sim` | `make test-sim` | Any scenario in `tests/scenarios/` failing under `--driver sim` (T3) |
| `sanitizers` | `make test-sanitize` | ASan and UBSan on the native host build; Miri over the FFI crates' tests; loom over the refresh and queue paths |
| `coverage` | `make coverage` | A floor below missed (`cargo llvm-cov` over T1–T3) |
| `build-armv7` | `make build-armv7` | The device target doesn't cross-build with the pinned NDK, or the stripped binary exceeds its size budget |

**Coverage floors** are fixed numbers, not a ratchet file:

| Scope | Line | Branch |
|---|---|---|
| Whole workspace | 80% | 70% |
| `echo-mcp-auth`, `echo-mcp-client`, `echo-agent-loop`, `echo-gateway` | 90% | 85% |
| `echo-contracts`, `echo-config` | 95% | 90% |
| Lines a pull request changes | 85% | — |

Generated code and `native/` are excluded through one list in `.config/coverage.toml`; adding to it needs an ADR.

**Time budget.** Required checks finish in under 10 minutes with a warm cache. Each T1 test has a 30 s timeout and
each T3 scenario 120 s, on the fake clock. A check that outgrows its budget becomes its own plan item; it is never
made optional.

**Nightly and on label, never on the pull request path:**

| Job | Trigger | What fails it |
|---|---|---|
| `test-cloud` (T4) | Nightly, or the `cloud` label | A scenario under `--driver cloud` against GPT-Live and the Port OS pilot |
| `mutants` | Nightly | `cargo-mutants` score below 70% on `echo-mcp-auth` and `echo-agent-loop` |
| `deps-fresh` | Weekly | `cargo-deny` against the latest advisory database |

T5 and T6 never run in CI. The milestone report directory and `docs/program/milestones/<id>.md` are their record.

## 12. Phase order after M0

There are no factory waves or batches. The coordinator works the phases in order and fans out inside a phase only
where pieces are independent.

1. **W0 contracts.** `echo-contracts` (ids, `BoardEvent`, `AudioFrame`, `LiveEvent` generated from captured GPT-Live
   traces, `TaskEvent`, `UiState`, `UiAction`; `ElicitationRequest` waits for W2), traits and fakes, the board
   manifest probe. Done when T2 fixtures round-trip and the schema snapshot is stable.
2. **W1 parallel pieces.** Live session, WebRTC peer, C shim ABI, MCP client (Bearer), UI states, `echo-gateway`,
   `echo-lab` sim driver. Done when `m4-spoof-tool` passes with `--driver sim`. The two spikes in section 3 run
   here.
3. **W2 integration.** Agent loop, gateway model proxy, golden trace. Done when `m4-spoof-tool` passes with
   `--driver cloud`, and M5's scenario once the Port OS changes merge.
4. **B0 bench** runs in parallel from W0: the `echo-lab` device driver over the Port OS CLI, the STT oracle,
   `echo-lab calibrate`, and the Ring frame of the light ring.
5. **W3 device.** Device adapters on the probed firmware, then M1, M3, M5 and M6 as operator-started T5 runs.
6. **Muse** (`el-w3-muse-bridge`), gating M7.
7. **Optional:** `el-x-codex-spike`, after M4, gating nothing.

Contract changes after W0 are their own pull request, made by the coordinator, never a side effect of a feature.
