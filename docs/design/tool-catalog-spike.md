---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 20
status: measurement pending
---

# Port OS tool catalog measurement

The current bridge declares Port OS tools flat. The pin places them inside
the Responses Lite `functions` namespace alongside its native wrappers.
This replay compares that wire format with explicit domain namespaces and
with client-side search over deferred domain namespaces. All three use the
same tools, base instructions, model and fixed utterances.

## Pin behavior

Source paths below are relative to `codex-rs` at the
[a956835d pin](https://github.com/openai/codex/tree/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs).

| Surface | Pin evidence |
| --- | --- |
| Wire admission | `app-server-protocol/src/protocol/v2/thread.rs:145–151` exposes experimental `dynamicTools`. `protocol/src/dynamic_tools.rs:13–49` describes tagged functions and namespaces, with `deferLoading` in JSON and `defer_loading` in Rust. `protocol/src/dynamic_tools.rs:90–132` normalizes compatibility forms and rejects mixed encodings. |
| Native exposure | `core/src/tools/handlers/dynamic.rs:46–81,98–106` builds each callable, sets its deferred exposure, and provides its search metadata. `core/src/tools/spec_plan.rs:520–551` makes a configured direct-only namespace direct, including the stock `functions` namespace. |
| Deferred discovery | `models-manager/models.json:178,197,337` selects Responses Lite and enables search for `gpt-6.1-sol`. `model-provider/src/provider.rs:59–68` enables namespace tools for the Responses provider. `core/src/tools/spec_plan.rs:369–404,653–664` gates `tool_search` on both capabilities and the presence of a searchable deferred tool. |
| No-host dispatch | `core/src/tools/spec_plan.rs:808–850` exempts `excluded_tool_namespaces` from code-mode nesting. `core/src/tools/handlers/tool_search_spec.rs:99–108` declares client execution. `core/src/tools/router.rs:253–285` routes the client search and namespaced function call. `core/src/tools/handlers/tool_search.rs:226–240` returns the loaded definitions. |
| Captured input | `core/src/client.rs:902–930` emits the `additional_tools` input element. The search result travels in `tool_search_output` as defined by `protocol/src/models.rs:1170–1186`. |

The deferred replay groups tools under `portos_endpoints`, `portos_groups`
and `portos_subscriptions`. It excludes them from code-mode nesting so a
loaded tool can be called directly on an install without a code-mode host.
The namespaced direct replay uses those same namespaces in the direct-only
list. The replay also checks that the stock direct-only `functions`
namespace overrides a tool's deferred declaration.

## Full replay

Use a Linux checkout with the verified pin already provisioned, the
authorized local backend checkout, and offline `tiktoken` 0.13.0 with its
`o200k_base` vocabulary already cached. Keep the projection and request
captures in private storage. Set `backend` to the local backend checkout.

```sh
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
arrays. The fixed model peer derives an intent from the received utterance,
selects that tool only when visible, and waits for the correlated MCP receipt
before replying. The routing fixture exercises tool visibility and dispatch.
Provider billing, hosted model choices, and device behavior remain unmeasured.

The full replay is required to populate the comparison and recommend a
format. The hosted smoke uses two synthetic definitions and exercises the
same app-server boundaries, including candidate removal and a peer closing
with a model request pending.
