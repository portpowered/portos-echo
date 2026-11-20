---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 20
status: measured
---

# Port OS tool catalog measurement

The current bridge declares Port OS tools flat. The pin places them inside
the Responses Lite `functions` namespace alongside its native wrappers.
This replay compares that wire format with explicit domain namespaces and
checks whether client-side search can load deferred domain namespaces. The
variants use the same tools, base instructions, model and fixed utterances.

## Pin behavior

Source paths below are relative to `codex-rs` at the
[a956835d pin](https://github.com/openai/codex/tree/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs).

| Surface | Pin evidence |
| --- | --- |
| Wire admission | Generated experimental `v2/ThreadStartParams.json:362` exposes `dynamicTools`, lines 101 and 134 expose `deferLoading`, and line 189 identifies the namespace variant. `app-server-protocol/src/protocol/v2/thread.rs:145–151` wires in the normalizer. `protocol/src/dynamic_tools.rs:13–49,90–132` defines the tagged forms, accepts legacy forms and rejects mixed encodings. |
| Native exposure | `core/src/tools/handlers/dynamic.rs:46–81,98–106` builds each callable, sets its deferred exposure, and provides its search metadata. `core/src/tools/spec_plan.rs:520–551` makes a configured direct-only namespace direct, including the stock `functions` namespace. |
| Deferred discovery | `models-manager/models.json:178,197,337` selects Responses Lite and enables search for `gpt-6.1-sol`. `model-provider/src/provider.rs:59–68` enables namespace tools for the Responses provider. `core/src/tools/spec_plan.rs:369–404,653–664` gates `tool_search` on both capabilities and the presence of a searchable deferred tool. |
| Code-mode-only limit | `core/src/tools/spec_plan.rs:354–404` runs direct-only overrides before it adds `tool_search`. The handler inherits `Direct` exposure from `tools/src/tool_executor.rs:112–115`. The model-visible filter in `core/src/tools/spec_plan.rs:559–577,794–806` hides it because the code-mode predicate exempts only `exec` and `wait` (`code-mode-protocol/src/description.rs:259–261`). Code-mode registration also skips `ToolSpec::ToolSearch` (`core/src/tools/spec_plan.rs:808–869`). |
| Search wire path | `core/src/tools/handlers/tool_search_spec.rs:97–108` declares client execution. `core/src/tools/router.rs:253–285` routes a client search or a namespaced function call. `core/src/tools/handlers/tool_search.rs:226–240` returns loaded definitions when invoked. |
| Captured input | `core/src/client.rs:902–930` emits the `additional_tools` input element. The search result travels in `tool_search_output` as defined by `protocol/src/models.rs:1170–1186`. |

The deferred probe excludes `portos_endpoints`, `portos_groups` and
`portos_subscriptions` from code-mode nesting. The initial Responses Lite
request still contains `functions.exec` and omits both the deferred
declarations and client `tool_search`. This pin therefore offers no native
discovery route for that variant. Keep deferral off for the no-host setup.
The namespaced direct replay puts those domains in the direct-only list.
The pin requires an explicit namespace on deferred declarations and reserves
`functions` in `app-server/src/request_processors/thread_processor.rs:375–424`.
The test asserts both validation errors and exercises the same direct-only
override on a legal Port OS namespace. Under that override the deferred
declaration appears upfront and dispatches directly.

## Full replay

Use a Linux checkout with the verified pin already provisioned and set
`backend` to the authorized local backend checkout. Use a `python3` that has
`tiktoken==0.13.0` and the `o200k_base` vocabulary already cached for
`render`. The measured run used a Python virtual environment for this step.
Keep the projection and request captures in private storage.

```sh
set -eu
umask 077
work=$(mktemp -d)
catalog=$work/catalog.json
capture=$work/capture
backend_ref=5cdc18f70f66e2729e115eb989b3fae5cd276649
git -C "$backend" show "$backend_ref:backend/internal/transports/mcp/mcpspec/catalog.gen.go" |
  python3 -c '
import json, re, sys
table = sys.stdin.read().split("var toolDefinitions = map[string]string{\n", 1)[1].split("\n}", 1)[0]
tools = []
for line in table.splitlines():
    name, quoted = re.fullmatch(r"\s*\"([a-z_]+)\":\s*(\".*\"),", line).groups()
    definition = json.loads(json.loads(quoted))
    assert name == definition["name"]
    tools.append({key: definition[key] for key in ("name", "description", "inputSchema")})
sys.stdout.buffer.write(json.dumps(tools, ensure_ascii=False, separators=(",", ":")).encode())
' >"$catalog"
sha256sum "$catalog"
ECHO_CODEX_OFFLINE=1 ECHO_LAB_TOOL_CATALOG_FILE="$catalog" ECHO_LAB_TOOL_CATALOG_CAPTURE="$capture" \
  cargo test --locked -p echo-lab --bin echo-lab t3_session_tool_catalog -- --ignored --nocapture
python3 -B crates/echo-lab/src/lab_run/sim/session/catalog_measurement/replay.py render "$capture"
```

The projected ten-tool catalog has SHA-256
`7b357af7fa19565d83ef32b0bbee535020b9499a5910879e4ae2baafdae27f23`.
`endpoint_show` is included in the full profile with apps, groups and events
enabled. The backend's guide is a resource outside the measured tools.

`render` measures `encode_ordinary` on the exact UTF-8 spelling of each
`tools` array received by the fake model. It prints initial and follow-up
declaration counts, tokens in returned search definitions including repeats,
and total model requests. Counts include native wrapper declarations in those
arrays. `followup_additional` counts declarations sent again after the initial
request. `followup_search` counts declarations returned by search. The fixed model
peer derives an intent from the received utterance,
selects that tool only when visible, and waits for the correlated MCP receipt
before replying. The routing fixture exercises tool visibility and dispatch.
Provider billing, hosted model choices, and device behavior remain unmeasured.

## Results and recommendation

The full Linux replay at `548566989eaff70a599f0aa4c0b98a3edd8990d9` passed
`t3_session_tool_catalog`. The `render` command printed these asserted selections
and exact wire-token counts.

```text
variant case tool initial followup_additional followup_search round_trips
flat F01 endpoint_query 8077 8077 0 2
flat F02 endpoint_show 8077 8077 0 2
flat F03 endpoint_manage 8077 8077 0 2
flat F04 control 8077 8077 0 2
flat F05 group_query 8077 8077 0 2
flat F06 group_manage 8077 8077 0 2
flat F07 subscription_list 8077 8077 0 2
flat F08 subscription_inspect 8077 8077 0 2
flat F09 subscription_cancel 8077 8077 0 2
flat F10 subscription_label_edit 8077 8077 0 2
namespaced F01 endpoint_query 8161 8161 0 2
namespaced F02 endpoint_show 8161 8161 0 2
namespaced F03 endpoint_manage 8161 8161 0 2
namespaced F04 control 8161 8161 0 2
namespaced F05 group_query 8161 8161 0 2
namespaced F06 group_manage 8161 8161 0 2
namespaced F07 subscription_list 8161 8161 0 2
namespaced F08 subscription_inspect 8161 8161 0 2
namespaced F09 subscription_cancel 8161 8161 0 2
namespaced F10 subscription_label_edit 8161 8161 0 2
```

Keep the current flat direct definitions on this pin. Explicit domain
namespaces add 84 tokens to each declaration block (about 1%) and 168
tokens per measured turn. Both formats select the expected tool in all
ten cases, issue one correlated MCP call per case, and take two model
requests. The fixture measures visibility and dispatch, so it gives no
estimate of how domain labels affect a hosted model's selection accuracy.
Keep deferred loading off in the no-host setup. The deferred probe has no
selection or comparable token count because the pin hides client
`tool_search` in code-mode-only turns.
