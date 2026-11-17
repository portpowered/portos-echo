---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: S5 established with the unmodified release; full client implementation pending
---

# Codex local fake: S5

The unmodified `rust-v0.160.0` release supports a wholly local WebRTC call-control
and delegated dynamic-tool journey with dummy API-key sign-in. No ICE, media,
real account, remote inference or milestone is exercised. The fixture supplies
synthetic SDP; it cannot establish a working media connection.

## Reproduce

Provision outside the test, then execute the component witness:

```sh
sh scripts/codex-app-server.sh
cargo test --locked -p echo-codex --test codex_s5 -- --nocapture
```

On Windows, provision using the PowerShell twin:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/codex-app-server.ps1
cargo test --locked -p echo-codex --test codex_s5 -- --nocapture
```

For offline schema generation, use the helper's returned executable:

```sh
binary=$(sh scripts/codex-app-server.sh)
"$binary" app-server generate-json-schema --experimental --out /tmp/codex-selected-schema
```

The Linux test re-executes its already available Python fixture under
`unshare --user --map-root-user --net`. It brings only `lo` up, asserts the
interface inventory and empty routes, then verifies a connection to the
documentation-only address `192.0.2.1` is refused. Missing namespace support
fails the witness. Windows proves portable routing and the refusing proxy;
it does not claim Linux confinement. The fixture uses allocated IPv4 loopback
ports and a fresh external CODEX_HOME/cwd, with an allowlisted child environment.
No inherited provider credential is passed. Both platforms set a refusing
HTTP/HTTPS/ALL proxy and `NO_PROXY=127.0.0.1`.

## Supported redirects and settings

Set these process-local app-server overrides (`PORT` is allocated by the fake):

```toml
model_provider = "local"
model = "gpt-5.4"
experimental_realtime_webrtc_call_base_url = "http://127.0.0.1:PORT/backend-api/codex"
experimental_realtime_ws_base_url = "http://127.0.0.1:PORT/v1"
web_search = "disabled"
mcp_servers = {}
cli_auth_credentials_store = "file"
check_for_update_on_startup = false
analytics.enabled = false
feedback.enabled = false
cloud.skills.enabled = false
tools.update_plan.enabled = false
tools.experimental_request_user_input.enabled = false
features.shell_tool = false
features.unified_exec = false
features.multi_agent = false
features.multi_agent_v2 = false
features.goals = false
features.plugins = false
features.recommended_plugins = false
features.remote_models = false
features.apps = false
features.remote_control = false
features.skill_search = false
features.skip_host_skill_discovery = true

[model_providers.local]
name = "local"
base_url = "http://127.0.0.1:PORT/v1"
wire_api = "responses"
experimental_bearer_token = "dummy-local-only"
```

`thread/start` additionally uses `ephemeral:true`, `approvalPolicy:"never"`,
`sandbox:"read-only"`, `environments:[]` and only the `echo_test` dynamic tool.
Start explicitly supplies `version:"v3"`, `outputModality:"audio"`, a WebRTC
offer, `includeStartupContext:false` and `clientManagedHandoffs:false`.
A synthetic `prompt` prevents copying the pin's default prompt into observations.

## Wire observations and exact oracle

Both verified Windows and Linux archives produce the following synthetic journey:

- Initialize acknowledges `experimentalApi:true`; dummy `apiKey` sign-in succeeds.
- Exactly one POST reaches
  `/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas`.
- The offer is exactly `v=0\r\ns=fake-offer\r\n`. The release's actual session
  model is **`gpt-live-1-codex`**, rather than the research brief's
  `gpt-live-1.1-codex`. The packet permits recording the actual pinned value;
  the test asserts this exact observed value without overriding the model.
- The fake returns HTTP 201, `Location:/v1/live/rtc_local` and exactly
  `v=0\r\ns=fake-answer\r\n`. One matching SDP notification and started `v3`
  reach the caller.
- Exactly one WebSocket connects to `/v1/live/rtc_local`. A synthetic
  `delegation.created` for `delegation-local` starts the delegated model turn.
- Two POSTs reach `/v1/responses`: function call then final message. Both list
  exactly `[echo_test]`. One `item/tool/call` has tool `echo_test`, arguments
  `{"message":"ping"}` and call ID `tool-local`. The reply uses the incoming
  request ID, and one matching function-call output reaches the second request.
- Exactly one `delegation.context.append` has
  `delegation_item_id:"delegation-local"` and content
  `[{"type":"input_text","text":"echo-ok"}]`.
- Stop yields `thread/realtime/closed`; the proxy observes zero external
  requests. The witness prints `S5_PASS` and exits zero.

Before trimming, the same model exposed `request_user_input`, the multi-agent
namespace and `get_goal`, `create_goal`, `update_goal`, alongside `echo_test`.
The config above removes them without patching Codex. Empty environments remove
apply_patch. Disabling only shell/unified-exec leaves the other built-ins.
Before disabling plugin/catalog discovery, the refusing proxy observed external
connection attempts; those attempts were refused locally. The final fixture
asserts zero, rather than treating refusal as production acceptance.

The pinned source confirms the redirect: `core/src/realtime_conversation.rs`
falls back to `experimental_realtime_ws_base_url` for its sideband base, then
passes it to `with_webrtc_sideband_base_url`. `core/src/tools/spec_plan.rs`
requires an environment before adding apply_patch. These references are to the
pinned commit, not Codex main. This resolves the S5 feasibility questions only.

## Remaining proof

The current witness drives raw stdio from Python. It does not exercise a Rust
client or prove its bounded lines, IDs, queues, deadlines, restart budget,
kill/reap/drop, concurrent session isolation, error matrix or missing-v3 seed.
Those remain retained work in `el-w2d-codex-client-001`; the final public-client
journey must reuse the real binary/local-fake boundaries. Independent review,
final quality checks, merge and loopback remain review-owned. Real authentication,
bridge/MCP integration and M2/M4 belong to their later gates.
