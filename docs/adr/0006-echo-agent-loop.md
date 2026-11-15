---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: provisional; local Responses component, real provider acceptance pending M2
---

# 0006: Bridge-side Responses agent loop

## Decision

Implement the existing generic `AgentBackend` and `ToolHost` traits in
`echo-agent-loop`, using one reused reqwest 0.12 client with native-root rustls.
Disable default features, ambient proxies, redirects and automatic retries.
Use `Response::chunk` directly for SSE; no model SDK, OAuth library or SSE framework.
Tokio owns task workers, bounded two-event storage and notification. Per-task polling
is serialized with terminal transitions; cancellation aborts and joins the worker
before returning. Deadlines cover catalog, credentials, HTTP and tools from submit,
using the minimum of caller and configured limits (including zero caller limits).
One mutex-protected UTC-day counter reserves each POST after credential validation;
failed and cancelled dispatched attempts remain charged. Accounting is per backend
instance, not durable across restarts. Drained tasks retain only issued IDs. Serde JSON
preserves opaque completed response items without projecting away reasoning fields.
Credentials arrive locally through `ModelCredential` for each POST, wrapped in
`Secret<HeaderMap>`. Required headers are validated and marked sensitive. Errors
use fixed categories and never include provider bodies, headers or input values.

## Provisional wire contract

The fixture contract follows the completed-item path of pinned Codex
`ee344ca9458351a6ea102795fa73110ad3ede5dc`, not a captured provider trace.
`chatgpt` selects `/backend-api/codex/responses` at chatgpt.com; `api_key`
selects `/v1/responses` at api.openai.com. Only ChatGPT requests carry
`chatgpt-account-id`; no Live-specific Alpha header is introduced.
A test constructor changes only the origin to numeric 127.0.0.1 over HTTP.
Production URLs are asserted offline; no provider is contacted in this lane.

Task instructions become developer text and context becomes compact JSON user text.
Catalog order maps original names to `tool_0`, `tool_1`, preserving exact descriptions
and schemas with `strict=false`. Dispatch maps back to the original host name.
Completed output items are replayed in index order, followed by correlated serialized
`ToolResult` outputs. Keys combine task ID and provider call ID. Calls run sequentially;
argument deltas never execute. A completed response with no pending calls supplies
only its assistant `output_text` blocks, concatenated without inserted separators.
Malformed, duplicate, failed, incomplete or truncated streams fail honestly.

## Configuration and compatibility

`agent.model` is optional when loading legacy TOML, with no runtime default.
An explicit value is preserved exactly; blank strings and wrong types have safe
field diagnostics. Backend construction requires a nonblank model before any
credential acquisition or connection. Existing 8-tool, 60-second and 500-request
config defaults and validation ranges remain unchanged. No config network or
runtime dependency is added. The example configuration stays unchanged.

## Scope and verification

Story 001 establishes one/two tool turns and a direct answer with an empty catalog
in both credential modes. T3 component tests use the real HTTP client on ephemeral
127.0.0.1 with `FakeToolHost` and rotating synthetic credentials. T1/T2 tests protect
config compatibility, endpoint mapping, headers, fragmented UTF-8/CRLF framing,
opaque item preservation and protocol rejection. Commands:

~~~sh
cargo test --locked -p echo-config -p echo-agent-loop --lib -- --nocapture
cargo test --locked -p echo-config --test config_load
cargo test --locked -p echo-agent-loop --test t3_agent_loop -- --nocapture
make ci
~~~

Story 002 extends this same lane/PR with daily accounting, deadlines, cancellation
arbitration and F04-F24. Readiness and disconnect/drop signals prove cleanup of
stalled operations; an injected day source and paused Tokio time cover rollover
and deadlines without real services or fixed sleeps. Later gates W2-BRIDGE and W2-M4-SIM own actual MCP and bridge
integration; M2-REAL and M4-M5-REAL own separately authorized real-service/device
validation. No paid calls, real credentials or hardware runs are authorized.
R-FOCUSED, R-CI, R-MERGE and V-LOOP remain owned by review.

Rollback removes the crate and its registrations, regenerates Cargo.lock, and removes
`agent.model` from operator TOML before using the older unknown-key-rejecting loader.
There is no shared contract migration or durable state in this crate.

## Packages covered

The following package names are newly introduced by Cargo resolution, including
inactive and platform-specific entries. Existing direct dependencies are already
covered by prior ADRs. No license/advisory exception is introduced.

- `atomic-waker`
- `cfg_aliases`
- `core-foundation`
- `core-foundation-sys`
- `echo-agent-loop`
- `form_urlencoded`
- `futures-channel`
- `http`
- `http-body`
- `http-body-util`
- `httparse`
- `hyper`
- `hyper-rustls`
- `hyper-util`
- `ipnet`
- `lru-slab`
- `openssl-probe`
- `quinn`
- `quinn-proto`
- `quinn-udp`
- `rand_pcg`
- `reqwest`
- `ring`
- `rustls`
- `rustls-native-certs`
- `rustls-webpki`
- `schannel`
- `security-framework`
- `security-framework-sys`
- `serde_urlencoded`
- `sync_wrapper`
- `tinyvec`
- `tokio-rustls`
- `tower`
- `tower-http`
- `tower-layer`
- `tower-service`
- `try-lock`
- `url`
- `want`
- `wasm-bindgen-futures`
- `web-sys`
- `web-time`
- `windows-targets`
- `windows_aarch64_gnullvm`
- `windows_aarch64_msvc`
- `windows_i686_gnu`
- `windows_i686_gnullvm`
- `windows_i686_msvc`
- `windows_x86_64_gnu`
- `windows_x86_64_gnullvm`
- `windows_x86_64_msvc`
