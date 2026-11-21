---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 20
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
| `home/skills/` | Optional owner skills exposed to the delegated runner through its `read_skill` tool. |

For a manual host placement, nominate equivalent paths outside the repository and preserve the same private modes.
The installer does not rewrite the old binary/CA paths in the Echo example; use the corrected `bin/` paths in
the quickstart. It rejects unsafe paths and migrates retained private state to the fixed non-root owner.

The base must validate first. If `[home]` is configured, `BridgeConfig::load_effective` then reads that home's
optional `settings.toml` (maximum 65,536 bytes). The overlay can set these leaves:

- Voice and OpenAI use `voice.session_listen_enabled`, `voice.session_listen_seconds`,
  `codex.voice`, `codex.web_search`, `codex.delegate_model`, `codex.delegate_effort` and `codex.delegate_fast`.
  An empty delegated model or effort selects the pinned app server's default.
- Agent settings include `agent.daily_request_cap`
  and the `notes` and `prompts` groups in `home.agent_tools`. An empty group list disables both groups.
- Port OS permissions use `mcp.scopes` and the `allow_tools` list under `[[mcp.server]]`, bound to the configured
  Port OS server by its `name`. An empty tool list disables all Port OS tools.
- Port OS endpoint Settings stores `portos_endpoint.enabled` and `portos_endpoint.device_name`.
- Muse Settings stores an HTTPS origin and references to its private CA and credential files in `[muse]`.
- The root `screen_pack` selects installed screen packs.

Missing leaves retain base values. The Port OS server identity and URL are read-only in the panel.
Open the Settings gear. Accounts shows OpenAI (GPT Live) and Port OS. To link either account, open the displayed
verification URL on a phone or PC and enter the code before it expires. Confirm Unlink to remove the selected
account's stored sign-in. OpenAI account changes wait for the current conversation to settle, then update the
running bridge. If the bridge uses an API key, manage that credential in the host configuration.
Port OS permissions shows the requested scopes next to the current grant. After saving scopes, wait for Settings
to report that the saved values are active before linking again. Presentation lists the installed screen packs
and puts Template eyes first. Omitting `screen_pack` uses the installed template.
Unknown keys, bad types, unreadable/oversized files or invalid effective values reject the **whole overlay**;
the validated base remains active and a warning is presented. An invalid base remains a startup error.
The operator account/link commands load base directly; the voice runtime loads the effective configuration.

Settings edits persist first, then request replacement at an idle boundary between calls.
Withdrawing Port OS scopes or tools also denies affected calls in the running bridge as soon as the save succeeds.
Adding permissions takes effect after restart and still requires the applicable grant scopes.
Settings edits let active conversations finish. Presentation saves use the same atomic writer and update the
screen without restarting the bridge. See [screen packs](screen-packs.md).
Pending/applied and failed-restart state are shown on screen. After replacement failure,
the old active configuration remains usable; a later explicit stop/start loads the saved overlay.
Other base edits need restart. To remove an override, stop and edit/remove the corresponding overlay entry;
changing the base alone does not win over a saved screen value.

### Port OS endpoint

The endpoint preference defaults to off with the name **Echo Show**. From paired Settings, open
**Port OS**, edit **Name in Port OS** or **Connect this Echo to Port OS**, and save. Names must
contain 1 to 64 Unicode characters, including at least one non-whitespace character and no control
characters. The bridge saves these preferences without restarting a conversation.
Use the existing **Port OS** link to approve the Echo once. Both MCP and the endpoint use
that sign-in. The bridge requests a separate access token for each audience.

```toml
[portos_endpoint]
enabled = false
remote_mic = false
device_name = "Echo Show"
gateway_url = "wss://gateway.example.invalid/devices/{authLinkId}"
api_base = "https://api.example.invalid"
```

Set network destinations in the base file. The optional WSS template ends in
`/devices/{authLinkId}`. The API base defaults to the configured Port OS grant's HTTPS resource
origin. Omit inline credentials, queries, fragments and other substitutions from both. Settings shows
an unavailable state until the endpoint issuer and an authoritative route resolver are available.

Remote and wake sessions share one voice slot. A text start can speak a reply without opening
device capture. Set `remote_mic = true` only for a Wyoming peer that acknowledges the correlated
capture request. A mic-only start then waits up to two seconds for the peer. A text start that
requests follow-up tries capture after its first reply has played. An explicit remote end cancels
delegated work and closes playback and capture.

Port OS speaker commands request absolute volume, a relative change from the last reported volume,
or output mute. The current Wyoming peer does not acknowledge these controls, so the bridge replies
`NOT_SUPPORTED`. Volume and output mute are unknown until the device reports them. Microphone
privacy continues to control the assistant's muted state independently.

Snapshot requests use the camera source's own shutter state. A closed shutter refuses capture,
including if it closes during capture. A supported source returns one in-memory JPEG with its
dimensions and sample time. The entire encoded reply is capped at 8 MiB. The bridge returns
`NOT_SUPPORTED` until a camera source is installed.

To downgrade, remove this section from the base file and Settings overlay.

### Muse

In the paired Muse section, enter the bridge origin, the CA as base64-encoded DER and the Muse service
credential. Saving a new origin requires both private inputs. At the same origin, leave a private input blank
to preserve it. Settings stores the inputs in owner-only files under `home/muse/` and returns only the accepted
origin and connection status. Test connection checks the saved origin once with an authenticated request
and submits no task. Changes apply after the idle restart. Clear Muse immediately withdraws `ask_muse`
even if replacement fails, and saves `[muse] enabled = false` to mask any inherited base connection.
To restore a prior connection, stop the bridge and restore its overlay and matching files together.
Before downgrading to a bridge without Muse Settings support, remove the overlay's `[muse]` section.

Prompts are separate UTF-8 files, not TOML keys: `prompts/voice.md` guides the conversation and
`prompts/runner.md` guides delegated turns. Reloads apply at new call/thread boundaries, not mid-turn.
Missing files use built-ins; invalid edits keep the prior accepted text. The reserved `settings.toml`
is never a prompt. The removed `codex.instructions` and `codex.live_instructions` keys are rejected with
migration diagnostics naming these prompt files.

An invalid prompt on initial startup has no previously accepted text to retain and fails admission;
correct it before starting. The retain-previous behavior applies to reloads of a running owner.

### Owner skills

Copy the [sample skill](../examples/skills/brief/SKILL.md) to `skills/brief/SKILL.md` under the directory
configured in `[home].dir`. Set `skills = ["brief"]` in the base `[codex]` table and restart.
Use a UTF-8 Markdown file with `---` delimiters around the front matter and one line each for `name`
and `description`. The name must exactly match its folder and its entry in the selection.
Use distinct folder names without path separators or `..`. Use ordinary directories and files.
The private Codex home is unsupported for owner skills.

The delegated runner sees the enabled names and descriptions and can call `read_skill` for their Markdown
bodies. The body starts after the closing delimiter's line ending and retains its original whitespace.
The bridge reads the enabled files at startup, so restart after changing either a file or the selection.
Omit `skills` or set `skills = []` to disable the catalog and tool. An unknown selected name reports its
indexed key, such as `codex.skills[0]: unknown skill`, and exits with status 2. Remove the key before rolling
back to an older bridge binary.

The encoded catalog may use up to half of the Codex RPC frame (512 KiB). The rest is available for the
runner prompt, settings, and tool declarations. An oversized selection reports `codex.skills` at startup.

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

In paired Settings, open OpenAI, select **Delegated fast mode**, and choose **Save settings**. Wait for
Settings to report that the saved values are active. The next runner thread starts with the saved tier.
Fast mode requests priority processing for a supported runner model. The pin's bundled catalog advertises
priority for `gpt-6-astra`, `gpt-6.1-sol`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`,
`gpt-5.6-terra`, `gpt-5.6-luna` and `gpt-5.5`. The provider controls eligibility, cost and speed.
The Live voice uses its own start parameters.

For standard processing, clear the checkbox and save, or set `delegate_fast = false` under `[codex]` in TOML.
A saved false overrides a true base value. To return to the base preference, stop the bridge, remove just
`delegate_fast` from the home's `settings.toml`, and start the bridge again. Before downgrading to a bridge
without this key, remove it from both TOML files.

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
unified execution and independent Codex MCP discovery disabled. Provider-backed read-only web search is the sole
exception: `codex.web_search` defaults to `live`, accepts `cached` or `disabled`, and adds no local authority.
Conventional models use hosted search; native Responses Lite models use the pin's provider-backed `web.run`,
which runs behind `exec` and so is unavailable on the Echo until `codex-code-mode-host` is built for it.
Cached sets `external_web_access` false for both forms; disabled omits search.
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

`agent.daily_request_cap` limits delegated requests by UTC day. New requests above that cap are refused and the
allowance resets at UTC midnight. The bridge stops waiting on a stalled delegation at the configured session
maximum. The daily request cap applies to delegated requests.
Quiet-hours options in `echo-lab` govern the operator bench; there is no accepted bridge quiet-hours schedule key.
Use stop/mute for operator control and preserve the hardware mute latch's authority.

Start with narrow read scopes, a small explicit `allow_tools`, local home groups disabled unless needed,
the default caps, loopback listeners and private credentials. The example includes `control`; remove it for
a read-only installation.

When you link Port OS with `mcp.client_id` omitted, the bridge registers its own public client. The approval page
uses the configured `mcp.client_name`, which defaults to `Port OS Echo`. Set `mcp.client_id` to use an existing
registered public client.

Settings > Accounts > Port OS advises relinking when the current grant uses the built-in CLI client. If an older
installation set `mcp.client_id` to that client, remove the line and restart the bridge before relinking. The
existing grant keeps its original client until you approve a replacement link or remove the link.

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
| `home.agent_tools` | Default true (notes and prompts); false or [] disables, or array containing unique `notes` and/or `prompts`. | boundary / yes |
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
| `voice.session_max_seconds` | Integer 3–2147483647, default 600; run ceiling for delegated waiting and agent-granted listening extensions, at least listen seconds. Active speech and playback finish under their existing rules. | restart / no |
| `voice.speech_start_expiry_seconds` | Integer 3–300, default 20; how long a user speech start with no transcript text counts as speech. The pinned V3 session sends no speech-stopped, so without text the latch expires and the no-answer, idle and listen deadlines apply again. Speech also holds the `voice.session_max_seconds` cap by this long at most. | restart / no |
| `audio` | Optional audio policy table. | restart / no |
| `audio.reply_lead_ms` | Positive u16 multiple of 20 milliseconds, default 200; maximum send lead. | restart / no |
| `clock` | Optional network-time table. The bridge measures its clock against SNTP servers and tells the model the measured time while the system clock is more than 2 s off; see [architecture](architecture.md), launch contract item 9. | restart / no |
| `clock.servers` | Array of at most 8 `host` or `host:port` names (port 123 when omitted). Default: `0.pool.ntp.org` to `3.pool.ntp.org`. `[]` turns measurement off. | restart / no |
| `clock.interval_seconds` | Integer 60–86400, default 3600; time between measurements after the one at startup. | restart / no |
| `agent` | Required delegated-task policy table. | restart / no |
| `agent.backend` | Removed. Delete this key and configure the optional `[muse]` table to enable Muse. | rejected |
| `agent.daily_request_cap` | Unsigned 64-bit integer, default 500; UTC-day delegated-request cap. Overlay must also fit signed TOML integer. | boundary / yes |
| `agent.model` | Optional nonblank runner-model alias, lowest precedence. | restart / no |
| `muse` | Optional connection for the Codex voice tool `ask_muse`. The bridge advertises it after loading a usable CA and credential. Tasks use the remaining Wyoming run time. | restart / yes |
| `muse.url` | Required HTTPS origin without credentials, query, fragment or non-root path; optional valid u16 port. | restart / yes |
| `muse.ca_file` | Required nonblank literal CA path. | restart / no |
| `muse.credential_file` | Required nonblank literal credential path. | restart / no |
| `muse.enabled` | Settings overlay only. A lone `false` masks the base Muse connection and immediately withdraws the tool. | withdraw immediately / yes |
| `mcp` | Required server/grant table. | restart / no |
| `mcp.state_dir` | Optional nonempty path; runtime Port OS requires private state outside a repository, no parent traversal. | restart / no |
| `mcp.client_id` | Optional override for an already registered public client. Omit it to register this Echo when you link Port OS. Accepts nonblank text without control characters. | restart / no |
| `mcp.client_name` | Display name for automatic registration. Defaults to `Port OS Echo`. Accepts nonblank text without control characters. | restart / no |
| `mcp.scopes` | Defaults to all seven supported Port OS scopes; nonempty array of nonempty names without whitespace/control characters. Grant consent can further constrain access. | withdraw immediately, other changes at boundary / yes |
| `mcp.operator_listen` | Socket address on literal IPv4 loopback, default 127.0.0.1:16902; local grant-owner commands. | restart / no |
| `mcp.server` | Optional array of tables, default empty. | restart / no |
| `mcp.server[]` | One server table per element; runtime Port OS grant wiring selects the first server when it uses portos_device_code. | restart / no |
| `mcp.server[].name` | Required string, unique across configured servers. | restart / no |
| `mcp.server[].url` | Required HTTP(S) URL without inline credentials, whitespace or backslash; valid host/optional u16 port. | restart / no |
| `mcp.server[].auth` | Required: `none`, `portos_device_code`, or `bearer_file`. | restart / no |
| `mcp.server[].bearer_file` | Optional literal file path; required with bearer_file auth. Store credential privately. | restart / no |
| `mcp.server[].allow_tools` | Array of exact tool-name strings, defaults to all eleven Port OS tools for device-code auth and empty for other auth modes. An explicit empty array disables all tools. | withdraw immediately, other changes at boundary / yes |
| `portos_endpoint` | Optional Port OS endpoint table. | see individual keys |
| `portos_endpoint.enabled` | Boolean, default false. Opts in once the endpoint service prerequisites are available. | save / yes |
| `portos_endpoint.remote_mic` | Boolean, default false. Enable only when the Wyoming peer supports the correlated remote capture handshake. | restart / no |
| `portos_endpoint.device_name` | Defaults to `Echo Show`. A nonblank name of 1 to 64 Unicode characters without controls. | save / yes |
| `portos_endpoint.gateway_url` | Optional WSS template ending in `/devices/{authLinkId}`. | restart / no |
| `portos_endpoint.api_base` | Optional HTTPS API base. Defaults to the configured Port OS grant's HTTPS resource origin. | restart / no |
| `screen` | Required screen transport table. | restart / no |
| `screen.listen` | Socket address; decoder defaults to all IPv4 interfaces on port 16900. Runtime device config must use 127.0.0.1:16900. | restart / no |
| `screen.pairing_token_file` | Required literal private token-file path. | restart / no |
| `screen.packs` | Optional pinned screen archive list, default `[]`; each entry has exactly HTTPS `url` and 64-hex `sha256`. Requires home when nonempty; background installation uses only `codex.ca_file` roots. See [screen packs](screen-packs.md). | restart / no |
| `screen.mouth_delay_ms` | Integer 0 to 1000, default `40`: milliseconds from a reply frame's place in the Echo's queue until the page should start drawing it (the Echo's output latency, measured at about 85 ms, less the page's drawing time of about 35 ms, with the picture left about 15 ms early). Lip-sync samples are stamped this far after the hand-off. Raise it when the mouth leads the voice, lower it when the mouth trails. See [screen packs](screen-packs.md). | restart / no |
| `screen_pack` | Sparse settings.toml root only: omitted chooses highest installed template, empty string disables packs, exact local name or name@version selects installed data. Never triggers a voice restart. | selection save / no |
| `audit` | Required audit table. | restart / no |
| `audit.log_dir` | Required literal directory path; runtime creates private audit files. | restart / no |
| `trace` | Optional private delegation trace table; omitted or empty uses defaults. Remove before restoring an older binary. | restart / no |
| `trace.payloads` | Boolean, default `true`; `false` disables task/request/response/answer capture while retaining lifecycle metadata. Credentials and account/installation identifiers are redacted; captured objects are capped at 64 KiB. Invalid types and unknown trace keys fail offline with sanitized diagnostics. | restart / no |
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
| `codex.web_search` | `live` default (also with table/key omitted), `cached` or `disabled`. Provider-backed search only (hosted or native `web.run`); all other tool restrictions stay intact. Invalid type/value: `codex.web_search: invalid type or value`, exit 2 before runtime effects. Set disabled and restart to turn off; remove key before restoring an older binary. | boundary / yes |
| `codex.voice` | Default unset; overrides live.voice. `default` omits outgoing field; other choices below, nonblank. | boundary / yes |
| `codex.model` | Optional nonblank legacy runner-model alias; overrides agent.model. | restart / no |
| `codex.delegate_model` | Optional nonblank runner model; overrides both older model keys. An empty screen override selects the pinned app server default even when a legacy model key is set. | boundary / yes |
| `codex.delegate_effort` | Optional nonempty string sent to runner; decoder imposes no enum, pinned app server validates supported effort. | boundary / yes |
| `codex.delegate_fast` | Boolean, default false. True requests priority processing for the delegated model. False selects standard routing. | boundary / yes |
| `codex.time_zone` | Default unset; IANA-name syntax (nonempty slash components using ASCII letters/digits/underscore/plus/minus). Not a lookup validating that zone exists. | restart / no |
| `codex.skills` | Default []; ordered list of distinct owner skill names under `home.dir/skills`. Nonempty requires the configured Codex client runner and agent home. | restart / no |

Set `codex.time_zone` to the operator's IANA time zone for time answers.
Run `adb shell getprop persist.sys.timezone` to see the device zone.

Safe legacy endpoint URLs use HTTPS or literal IPv4 loopback HTTP, with no inline credentials, query or fragment.
Defaults are independent constants: changing issuer alone does not derive new defaults for other endpoints.
The public client defaults are defined in
[config_auth.rs](../crates/echo-config/src/config_auth.rs) and [lib.rs](../crates/echo-config/src/lib.rs).

Realtime voices: `default` plus every voice the running Codex app server lists for the realtime version the bridge
sends (`v3`). The bridge reads that list after sign-in, so Settings follows the bundled Codex release; `miko`,
`himari`, `hanul` and `leher` are among the 26 voices `rust-v0.171.0` lists. Until the list has been read, Settings
offers `arbor`, `breeze`, `cove`, `ember`, `juniper`, `maple`, `sol`, `spruce` and `vale`. Settings refuses to save a
voice that is not listed. A `codex.voice` in `bridge.toml` that the app server does not list is left out of the call,
with one line on stderr, and the app server's default voice speaks. The v2-only voices earlier builds offered (`alloy`,
`ash`, `ballad`, `cedar`, `coral`, `echo`, `sage`, `shimmer`, `verse`) still load from an existing `bridge.toml` or
`settings.toml`, as `default` with one settings warning; they are never offered or saved.
Offline, only the shape of a voice name is checked: lowercase ASCII letters, at most 32.
`live.voice` is a legacy fallback; use `codex.voice` for the screen editor.

## Checking changes

Run `make lint-docs` on a provisioned Linux host. Its coverage reader extracts full accepted paths from
production decoder modules, not the stale secret-diagnostic inventory. Missing canonical rows fail with
`docs/configuration.md: undocumented config keys: <sorted full paths>`.
Unsupported decoder syntax or missing source fails closed, requiring a checker update rather than silently
certifying partial coverage. The dedicated `scripts/test_lint_docs.py` self-tests exercise omission/restoration,
new accepted keys and unsupported input without building or running Echo.
