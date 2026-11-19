---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: active; accepted bridge configuration reference
---

# Echo configuration

Use the [quickstart](quickstart.md) for installation and the first sign-in.
This reference describes accepted TOML from `echo-config` and the current Codex voice backend.
Parsing accepts some legacy fields that this backend does not use; those are identified below.
Paths are literal: no tilde or environment expansion and no automatic credential discovery.

## Files, ownership and precedence

The installer root is `/data/local/echo-bridge`. Root owns `bin/` (0755); its executables are 0755 and
`bin/cacert.pem` is 0644. The bridge runs as `nobody` (9999:9999), groups `3990,inet`.
It and the child share that identity. Other processes using `nobody` share its filesystem privileges;
this is not a dedicated per-agent sandbox. The screen app receives public state and pairing access, not raw tokens.

| File or directory under the install root | Purpose and access |
| --- | --- |
| `private/bridge.toml` | Base config selected by `--config`; installer writes 0600 inside `private/` (0700), owned by the bridge user. No automatic search path. |
| `home/` | Non-secret agent home (0700), distinct from Codex home. The installer seeds `prompts/voice.md` and `prompts/runner.md` (0600) only if absent. |
| `home/settings.toml` | Sparse screen-owned overlay, private atomic writes (0600); never rewrites base. Optional when `[home]` exists. |
| `private/codex-home/` | `codex.home`, created/required 0700. App server stores ChatGPT sign-in in `auth.json` (file credential store, private 0600). Home lease allows one supervisor. |
| `private/live.json` | Nominated legacy Live credential path; current ChatGPT backend uses Codex home instead. In `api_key` mode the nominated file supplies the key; provision it privately, 0600. |
| `private/portos/grant.json` | Port OS grant/DPoP state (0600 in 0700 state directory); grant owner rotates and persists it. Not ChatGPT credentials. |
| `private/screen-pairing.txt` | Screen/control pairing credential, private 0600. Never embed it in a public guide or share a paired URL. |
| `private/audit/` | Audit directory 0700 with private 0600 records and `bridge-kill` state. Keep operational data private. |
| `private/bridge.trace`, `private/bridge.log` | Installer launcher trace and process output; private diagnostics, not microphone recordings. |
| `home/notes/` | Optional local notes exposed through the bridge's scoped home tools; keep credentials out. |

For a manual host placement, nominate equivalent paths outside the repository and preserve the same private modes.
The installer does not rewrite the old binary/CA paths in the Echo example; use the corrected `bin/` paths in
the quickstart. It rejects unsafe paths and migrates retained private state to the fixed non-root owner.

The base must validate first. If `[home]` is configured, `BridgeConfig::load_effective` then reads that home's
optional `settings.toml` (maximum 65,536 bytes). Only these leaves override base:
`voice.session_listen_enabled`, `voice.session_listen_seconds`, `codex.voice`,
`agent.daily_request_cap`, `agent.max_tool_calls`. Missing leaves retain base values.
Unknown keys, bad types, unreadable/oversized files or invalid effective values reject the **whole overlay**;
the validated base remains active and a warning is presented. An invalid base remains a startup error.
The operator account/link commands load base directly; the voice runtime loads the effective configuration.

Screen edits persist first, then request replacement at an idle/between-call boundary. They do not interrupt
an active conversation. Pending/applied and failed-restart state are shown on screen. After replacement failure,
the old active configuration remains usable; a later explicit stop/start loads the saved overlay.
Other base edits need restart. To remove an override, stop and edit/remove the corresponding overlay entry;
changing the base alone does not win over a saved screen value.

Prompts are separate UTF-8 files, not TOML keys: `prompts/voice.md` guides the conversation and
`prompts/runner.md` guides delegated turns. Reloads apply at new call/thread boundaries, not mid-turn.
Missing files use built-ins; invalid edits keep the prior accepted text. The reserved `settings.toml`
is never a prompt. The removed `codex.instructions` and `codex.live_instructions` keys are rejected with
migration diagnostics naming these prompt files.

An invalid prompt on initial startup has no previously accepted text to retain and fails admission;
correct it before starting. The retain-previous behavior applies to reloads of a running owner.

## Voice agent access and what it may do

The `chatgpt` voice path and the delegated Codex runner use the **same app-server ChatGPT sign-in**, kept in
the configured Codex home. Stop the running bridge, run `echo-bridge signin --config PATH` as its user, approve
the URL/device code and wait for `signed in`; then start the bridge. The command also presents the public code
on the screen and has a five-minute deadline. Denial/expiry requires a fresh attempt. A successful sign-in is
account authentication, not a guarantee that a Live call or delegated model is entitled.

An account must have enabled access to both the pinned Codex runner and the Live preview used by the app server.
The repository has no established subscription-tier guarantee for Live. Third-party use of the preview endpoints,
terms and account entitlement remain open under [plan section 4.5](program/plan.md#45-risks-the-operator-accepts-with-the-chatgpt-mode).
Do not assume that a particular paid plan, API subscription or a working Codex login guarantees Live eligibility.
The pinned server owns its realtime model; `codex.delegate_model` chooses the runner model, not the voice model.

In the current backend, desktop `import-codex` is refused. `signin --browser` is an alternate app-server flow
for a browser-capable host. `api_key` is an alternate credential mode: the bridge reads the nominated
`live.credential_file` as a key and passes it to `account/login/start`. ChatGPT `signin` is unavailable in that
mode. Never put a key or token in TOML, prompt text or command-line arguments.

There is no bridge `signout` verb. To remove local ChatGPT access, stop the bridge, remove only
`<codex.home>/auth.json` using the operator's private file management, then repeat sign-in if needed.
Do not edit the credential while the app server owns the home. Removing a file or purging the installation
does not revoke already issued provider credentials: revoke the associated authorization/session in the provider's
account controls separately. Port OS `unlink --config PATH` is a different operation, sent to the running grant
owner to revoke its grant; it does not remove the ChatGPT login.

The voice model answers conversation, explanations and general knowledge directly. It delegates when a request
needs an effect or unavailable live/personal data. The runner gets bridge-declared tools, with native shell,
unified execution and independent Codex MCP discovery disabled. Hosted read-only web search is the sole
exception: `codex.web_search` defaults to `live`, accepts `cached` or `disabled`, and adds no local authority.
A read-only runner sandbox does not prevent effects through explicitly granted MCP tools. Prompt instructions
guide behavior; allowlists and caps enforce the available effect surface.

For each MCP server, `allow_tools` names exact permitted tools; an empty list exposes none. Discovered tools
are filtered before exposure. The bridge executes effects, not the remote voice service. Local home tool groups
are controlled separately by `home.agent_tools`; notes/prompts access does not grant arbitrary host filesystem
or shell access. Disable groups you do not need.

The screen supports approve/deny for supported tool approval requests; denial/cancellation does not authorize
an effect. This is separate from the Port OS **phone** device-grant consent. Port OS MCP currently has no
per-tool elicitation requirement; supported approval UI (including spoof-MCP tests) does not mean every
Port OS `control` call prompts. Grant only the necessary scopes and tool names, not a broad permission list
on the assumption that a screen prompt will catch every action.

`agent.daily_request_cap` limits delegated requests by UTC day, `agent.max_tool_calls` bounds each task and
`agent.task_deadline_seconds` bounds its duration; over-budget work is refused/interrupted rather than silently
expanding permission. These are not a cap on all voice conversation or a provider billing guarantee.
Quiet-hours options in `echo-lab` govern the operator bench; there is no accepted bridge quiet-hours schedule key.
Use stop/mute for operator control and preserve the hardware mute latch's authority.

Start with narrow read scopes, a small explicit `allow_tools`, local home groups disabled unless needed,
the default caps, loopback listeners and private credentials. The example includes `control`; remove it for
a read-only installation. Use a dedicated Echo OAuth client before broader use as the decision log requires.

## Configuration reference

Each row is a canonical accepted full path used by the docs gate, derived from production `Fields` decoders,
including nested/array and settings-overlay readers. Do not replace rows with prose or fenced examples.
Unknown keys are rejected. All table containers below use TOML tables; `mcp.server[]` denotes an array of tables.
Defaults are decoder defaults, **not** necessarily the installed example values.

In the last column, **restart / no** means restart after a base edit and no screen editor.
**boundary / yes** means the screen can save an overlay and apply it at the safe boundary described above;
a manual base edit still needs reload/restart. Prompt editing is independent of these rows.

| Key | Meaning, default or required status, allowed values | Apply / screen |
| --- | --- | --- |
| `wake_earcon` | Wake sound; boolean, default true. | restart / no |
| `home` | Optional agent-home table; absent disables home-backed settings/tools/prompts. | restart / no |
| `home.dir` | Required when home present; nonempty absolute directory path. | restart / no |
| `home.agent_tools` | Default true (notes and prompts); false or [] disables, or array containing unique `notes` and/or `prompts`. | restart / no |
| `echo` | Required Wyoming table. | restart / no |
| `echo.host` | Required string host; use literal loopback on device. No additional hostname validation in decoder. | restart / no |
| `echo.wyoming_port` | Port integer 1–65535, default 16700. | restart / no |
| `live` | Required credential/Live policy table. | restart / no |
| `live.mode` | `chatgpt` (default) or `api_key`. | restart / no |
| `live.credential_file` | Required literal path; current ChatGPT backend instead uses Codex home; API-key backend reads this file. | restart / no |
| `live.voice` | Optional legacy voice fallback when codex.voice absent; pinned voice list below. | restart / no |
| `live.session_idle_seconds` | Unsigned 64-bit seconds, default 120; Live-call reuse idle timeout, not post-reply listening. | restart / no |
| `voice` | Optional run-listening table, defaults below. | restart / no |
| `voice.session_listen_enabled` | Boolean, default true; keep listening after reply. | boundary / yes |
| `voice.session_listen_seconds` | Integer 3–300, default 15; listening window. Must not exceed session_max_seconds. | boundary / yes |
| `voice.session_max_seconds` | Integer 3–2147483647, default 600; cap for agent-granted waiting extensions, at least listen seconds. Does not cut active speech/playback. | restart / no |
| `audio` | Optional audio policy table. | restart / no |
| `audio.reply_lead_ms` | Positive u16 multiple of 20 milliseconds, default 200; maximum send lead. | restart / no |
| `agent` | Required delegated-task policy table. | restart / no |
| `agent.backend` | `client` (default) or `muse` parses; current Codex voice runtime admits only client. | restart / no |
| `agent.max_tool_calls` | Integer 1–32, default 8, per delegated task. | boundary / yes |
| `agent.task_deadline_seconds` | Integer 5–600, default 60, per delegated task. | restart / no |
| `agent.daily_request_cap` | Unsigned 64-bit integer, default 500; UTC-day delegated-request cap. Overlay must also fit signed TOML integer. | boundary / yes |
| `agent.model` | Optional nonblank runner-model alias, lowest precedence. | restart / no |
| `muse` | Optional legacy backend table; required by muse selection; not used by current Codex voice path. | restart / no |
| `muse.url` | Required HTTPS origin without credentials, query, fragment or non-root path; optional valid u16 port. | restart / no |
| `muse.ca_file` | Required nonblank literal CA path. | restart / no |
| `muse.credential_file` | Required nonblank literal credential path. | restart / no |
| `mcp` | Required server/grant table. | restart / no |
| `mcp.state_dir` | Optional nonempty path; runtime Port OS requires private state outside a repository, no parent traversal. | restart / no |
| `mcp.client_id` | Default `port1/systems/zero/oauth-clients/command-line-interface` (public development client); nonblank text without controls. Use your registered client for broader deployment. | restart / no |
| `mcp.scopes` | Default ["endpoint:read"]; nonempty array of nonempty names without whitespace/control characters. Grant consent can further constrain access. | restart / no |
| `mcp.operator_listen` | Socket address on literal IPv4 loopback, default 127.0.0.1:16902; local grant-owner commands. | restart / no |
| `mcp.server` | Optional array of tables, default empty. | restart / no |
| `mcp.server[]` | One server table per element; runtime Port OS grant wiring selects the first server when it uses portos_device_code. | restart / no |
| `mcp.server[].name` | Required string, unique across configured servers. | restart / no |
| `mcp.server[].url` | Required HTTP(S) URL without inline credentials, whitespace or backslash; valid host/optional u16 port. | restart / no |
| `mcp.server[].auth` | Required: `none`, `portos_device_code`, or `bearer_file`. | restart / no |
| `mcp.server[].bearer_file` | Optional literal file path; required with bearer_file auth. Store credential privately. | restart / no |
| `mcp.server[].allow_tools` | Array of exact tool-name strings, default empty; only listed tools exposed. | restart / no |
| `screen` | Required screen transport table. | restart / no |
| `screen.listen` | Socket address; decoder defaults to all IPv4 interfaces on port 16900. Runtime device config must use 127.0.0.1:16900. | restart / no |
| `screen.pairing_token_file` | Required literal private token-file path. | restart / no |
| `audit` | Required audit table. | restart / no |
| `audit.log_dir` | Required literal directory path; runtime creates private audit files. | restart / no |
| `chatgpt_auth` | Optional legacy endpoint table, all defaults from config_auth.rs; current app-server ChatGPT path owns its own endpoints and ignores these overrides. | restart / no |
| `chatgpt_auth.issuer` | Default `https://auth.openai.com`; safe endpoint URL as below. | restart / no |
| `chatgpt_auth.client_id` | Default `app_EMoamEEZ73f0CkXaXp7hrann` (public Codex OAuth client); nonblank text, no controls. Not an account identifier or secret. | restart / no |
| `chatgpt_auth.scopes` | Default `openid profile email offline_access`; nonblank text, no controls. | restart / no |
| `chatgpt_auth.usercode_endpoint` | Default `https://auth.openai.com/api/accounts/deviceauth/usercode`; safe endpoint URL. | restart / no |
| `chatgpt_auth.device_token_endpoint` | Default `https://auth.openai.com/api/accounts/deviceauth/token`; safe endpoint URL. | restart / no |
| `chatgpt_auth.token_endpoint` | Default `https://auth.openai.com/oauth/token`; safe endpoint URL. | restart / no |
| `chatgpt_auth.authorization_endpoint` | Default `https://auth.openai.com/oauth/authorize`; safe endpoint URL. | restart / no |
| `chatgpt_auth.verification_url` | Default `https://auth.openai.com/codex/device`; safe endpoint URL. | restart / no |
| `chatgpt_auth.device_redirect_uri` | Default `https://auth.openai.com/deviceauth/callback`; safe endpoint URL. | restart / no |
| `chatgpt_auth.browser_redirect_uri` | Default `http://localhost:1455/auth/callback`; HTTP loopback (`localhost` or literal IPv4 loopback), path exactly /auth/callback. | restart / no |
| `codex` | Optional app-server table in decoder; current voice runtime requires home. | restart / no |
| `codex.home` | Default unset; absolute private directory, required at runtime, mode exactly 0700. | restart / no |
| `codex.binary` | Default unset (pinned host helper resolves multi-tool); if set absolute path. Device must name installed standalone app server. | restart / no |
| `codex.launch` | `subcommand` default (`app-server --listen stdio://`) or `standalone` (`--listen stdio://`); standalone requires binary. | restart / no |
| `codex.ca_file` | Default unset; absolute PEM path, child SSL_CERT_FILE and bridge Port OS OAuth/MCP additional trust roots. | restart / no |
| `codex.web_search` | `live` default (also with table/key omitted), `cached` or `disabled`. Hosted search only; all other tool restrictions stay intact. Invalid type/value: `codex.web_search: invalid type or value`, exit 2 before runtime effects. Set disabled and restart to turn off; remove key before restoring an older binary. | restart / no |
| `codex.voice` | Default unset; overrides live.voice. `default` omits outgoing field; other choices below, nonblank. | boundary / yes |
| `codex.model` | Optional nonblank legacy runner-model alias; overrides agent.model. | restart / no |
| `codex.delegate_model` | Optional nonblank runner model; overrides both older model keys. Unset leaves app-server default. | restart / no |
| `codex.delegate_effort` | Optional nonempty string sent to runner; decoder imposes no enum, pinned app server validates supported effort. | restart / no |
| `codex.time_zone` | Default unset; IANA-name syntax (nonempty slash components using ASCII letters/digits/underscore/plus/minus). Not a lookup validating that zone exists. | restart / no |

Safe legacy endpoint URLs use HTTPS or literal IPv4 loopback HTTP, with no inline credentials, query or fragment.
Defaults are independent constants: changing issuer alone does not derive new defaults for other endpoints.
The public client defaults are defined in
[config_auth.rs](../crates/echo-config/src/config_auth.rs) and [lib.rs](../crates/echo-config/src/lib.rs).

Pinned realtime voices: `default`, `alloy`, `arbor`, `ash`, `ballad`, `breeze`, `cedar`, `coral`, `cove`,
`echo`, `ember`, `juniper`, `maple`, `marin`, `sage`, `shimmer`, `sol`, `spruce`, `vale`, `verse`.
An unknown voice is rejected offline. `live.voice` is a legacy fallback; use `codex.voice` for the screen editor.

## Checking changes

Run `make lint-docs` on a provisioned Linux host. Its coverage reader extracts full accepted paths from
production decoder modules, not the stale secret-diagnostic inventory. Missing canonical rows fail with
`docs/configuration.md: undocumented config keys: <sorted full paths>`.
Unsupported decoder syntax or missing source fails closed, requiring a checker update rather than silently
certifying partial coverage. The dedicated `scripts/test_lint_docs.py` self-tests exercise omission/restoration,
new accepted keys and unsupported input without building or running Echo.
