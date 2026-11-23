---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 23
status: active; accepted bridge configuration reference
---

# Echo configuration

Use the [quickstart](quickstart.md) for installation and the first sign-in.
This reference describes accepted TOML from `echo-config` and the current Codex voice backend.
Parsing accepts some legacy fields that this backend does not use; those are identified below.
Paths are literal: no tilde or environment expansion and no automatic credential discovery.

## Files, ownership and precedence

The installer root is `/data/local/echo-bridge`. Root owns `bin/` (0755); its executables are 0755 and
`bin/cacert.pem` is 0644. The bridge runs as `nobody` (9999:9999), groups `3990,inet,input`.
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
  `codex.voice`, `codex.web_search`, `codex.delegate_model`, `codex.delegate_effort` and `codex.delegate_speed`
  (a stored legacy `codex.delegate_fast` still loads; the next save writes `delegate_speed`).
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
the validated base remains active. The startup warning appears on the paired screen and on stderr
(`private/bridge.log` with the installer launcher). `private/bridge.trace` records `settings.ignored`.
An invalid base remains a startup error.
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
that sign-in: it names both audiences (the API base and its `/mcp` resource), and the bridge
refreshes a separate access token for each. The bridge's own client registers and requests the
configured `mcp.scopes` plus `message:send`, `endpoint:write` and `endpoint:read`. A grant from
before this sign-in, or a client registered without those scopes, shows **Link** until it is
linked again; the bridge then registers a new client.

```toml
[portos_endpoint]
enabled = false
remote_mic = false
device_name = "Echo Show"
gateway_url = "wss://gateway.example.invalid/devices/{authLinkId}"
api_base = "https://api.example.invalid"
```

Set network destinations in the base file only to override the defaults. The API base defaults
to the configured Port OS grant's HTTPS resource origin, `https://api.portoperatingsystem.com`.
For that API the WSS template defaults to
`wss://devices.portoperatingsystem.com/devices/{authLinkId}`; any other API base needs
`gateway_url`, which ends in `/devices/{authLinkId}`. Omit inline credentials, queries, fragments
and other substitutions from both. Commands and reports address the Echo by its `deviceId`; the
bridge reads its route back for display and logs only. The read-back uses the same REST
API-resource token that posts `/messages`. Every page is `GET /routes?owner=~caller&maxResults=100`,
and the bridge follows each page's `paginationContext.nextToken`, sent back unchanged as the
`nextToken` query parameter. Port OS answers 400 for a continuation token that was modified, has
expired or was rebound. On that 400 the read-back starts again from page 1, at most once per
read-back. A 400 for page 1 is a rejection. A route matches only when its `associatedPluginRouteId`
is the `deviceId` sent at pairing and its `associatedAuthLinkId` is the current pairing's
`authLinkId`. A route that an earlier pairing left for the same `deviceId` therefore never matches.
The bridge sends a text `ping` on the gateway every 45 seconds. Close 4000 reconnects at once, 4503
backs off with the same token, 4401 retries with a refreshed token up to three times, 4403 stops
after one refreshed retry, and 4409 stops. Snapshot replies are always inline base64 JPEG stamped
with the capture time.

Remote and wake sessions share one voice slot. A text start can speak a reply without opening
device capture. Set `remote_mic = true` only for a Wyoming peer that acknowledges the correlated
capture request. A mic-only start then waits up to two seconds for the peer and returns `NOT_READY`
if it does not answer; without `remote_mic` it returns `NOT_SUPPORTED`. A text start that
requests follow-up tries capture after its first reply has played. An explicit remote end cancels
delegated work and closes playback and capture.

The bridge reports full state on every gateway connection, after it finds its route, and at least
every ten minutes while connected. It posts no state report while the gateway is disconnected:
Port OS marks the device `DISCONNECTED` 30 s after its socket is gone and `CONNECTED` when it
returns, and a `4000` (CYCLE) close reconnects at once. A state report that Port OS answers `NOT_FOUND`, before the
route exists, is sent again once the route is found. Each route read-back is traced as
`endpoint.route_attempt` with `outcome` (`none`, `found` or `error`), the failure `reason` and
`http_status`, and counts of `pages`, `results` and `routed` records (those that carry an
`associatedPluginRouteId`), and `restarts` (0 or 1). It never records ids.

Port OS speaker commands request absolute volume, a relative change from the last reported volume,
or output mute. They are synchronous: the bridge acks every one, even when the caller does not wait,
with one report carrying the frame's id as `requestId` and no other reply. After sending the change
it waits up to 3 s from receipt for the speaker state the device reports, and the ack carries that
state as `speaker` (`volume` and `muted`) with `sampledAtMs`. Otherwise the ack carries `error`
instead of `speaker`: `INVALID_REQUEST` for a bad body; `NOT_SUPPORTED` when the device reports no
speaker state (a hassmic older than the fork's speaker-state change, portpowered/echo-dot-assist#13,
applies neither and reports neither), and for a relative change with no reported base volume;
`NOT_READY` when no voice session holds a speaker link, the
link is busy, or the device reports state but did not reflect the change in time. With no caller
waiting (fire-and-forget, or past Port OS's 60 s) Port OS still applies the state and answers
404; that ack is done and never sent again. Each ack is traced as `endpoint.speaker_ack` with its
delivery `outcome` (`answered`, `not_waiting` or `expired`), its `code` (`OK` or the refusal) and
`http_status` (404 for `not_waiting`). A command that reaches no device is also traced as
`endpoint.speaker_dropped` with a fixed `reason` (`volume_unknown`, `no_speaker_link` or `busy`).
The audit row's code is the ack's. Speaker state reports without `requestId` stay plain state.
Volume and output mute are unknown until the device reports them. Microphone privacy continues to
control the assistant's muted state independently.

Over Wyoming the bridge sends `hassmic-set-volume` `{"volume": 0..100}` and
`hassmic-set-output-mute` `{"mute": bool}`. A hassmic with the speaker-state change reports
`hassmic-speaker` `{"volume": 0..100, "muted": bool}` on connecting, on every volume or output-mute
change whatever caused it (buttons, Settings, Sendspin, the bridge), and once in answer to each
set, even one that changed nothing; that report is the ack's `speaker`. `muted` is output mute:
while it is on the Echo plays silence and keeps its volume. It is not the microphone mute
(`hassmic-mute`). The bridge ignores a malformed report and forgets the state when the connection
changes, until the new connection reports. hassmic does not save output mute: a hassmic restart
unmutes the speaker.

Snapshot requests use the camera source's own shutter state. A closed shutter refuses capture,
including if it closes during capture. A supported source returns one in-memory JPEG with its
dimensions and sample time. The entire encoded reply is capped at 8 MiB. Without a camera source
the bridge returns `NOT_SUPPORTED`. While the screen app has not attached its camera stream, it
returns `CAMERA_BUSY`. The settings trace records each stream change as `endpoint.camera` with
`session` (`attached` or `detached`) and the fixed reason of the latest refused attach in
`refused` (`forbidden`, `unauthorized` or `conflict`).

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
Unset, the runner uses `gpt-6.1-sol`. gpt-6.2-* models currently refuse the direct `web.run` that search needs.

`codex.delegate_speed` chooses the runner's service tier: `standard` (default), `fast` or `ultrafast`.
In paired Settings, open OpenAI, choose **Delegated speed**, and choose **Save settings**. Wait for
Settings to report that the saved values are active. The next runner thread starts with the saved tier.
The bridge sends it as `thread/start` `serviceTier`: a present null for standard (which also clears a tier the
Codex home would otherwise inherit), `"priority"` for fast and `"ultrafast"` for ultrafast.
The Live voice uses its own start parameters.

- **Fast** requests priority processing. The pin's bundled catalog advertises priority for `gpt-6-astra`,
  `gpt-6.1-sol`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` and `gpt-5.5`.
- **Ultrafast** needs a runner model whose catalog lists the tier: `gpt-6-astra` or `gpt-6.1-sol`
  (also `gpt-6.2-astra` and `gpt-6.2-sol`, which currently refuse search) and the app server's
  `ultrafast_mode` feature, which is on by default. It costs about six times the standard API price;
  ChatGPT plans meter it at higher multipliers. The bridge does not refuse other models.

The provider controls eligibility, cost and speed. When the model or account cannot use the requested tier, the
app server serves the thread at standard without an error. The bridge then records a
`call.service_tier_downgraded` trace event with the requested and effective tiers, and the call continues.

For standard processing, choose **Standard** and save, or set `delegate_speed = "standard"` under `[codex]` in
TOML. A saved value overrides the base value. To return to the base preference, stop the bridge, remove just
`delegate_speed` (or a legacy `delegate_fast`) from the home's `settings.toml`, and start the bridge again.

The older boolean `delegate_fast` still loads in both files: `true` means `fast` and `false` means `standard`.
A file that sets both keys to different tiers is refused with
`codex.delegate_speed: conflicts with codex.delegate_fast; remove delegate_fast`. Settings saves only
`delegate_speed`, so a stored `delegate_fast` is replaced on the next save. A bridge older than this key
refuses `delegate_speed`: before downgrading, remove it from both TOML files, or replace it with
`delegate_fast` when the tier is `standard` or `fast`.

In the current backend, desktop `import-codex` is refused. `signin --browser` is an alternate app-server flow
for a browser-capable host. `api_key` is an alternate credential mode: the bridge reads the nominated
`live.credential_file` as a key and passes it to `account/login/start`. ChatGPT `signin` is unavailable in that
mode. Never put a key or token in TOML, prompt text or command-line arguments.

With an OpenAI API key, voice and delegated runner usage is billed to that key at API token rates, not to a ChatGPT
plan. The Ultrafast speed tier (`codex.delegate_speed`) needs the runner model `gpt-6-astra` or `gpt-6.1-sol` and is
billed at API rates, about six times the standard rate.

To install in this mode, set `[live] mode = "api_key"` and keep `live.credential_file` a plain file directly in
`/data/local/echo-bridge/private/` (the example's `live.json` works). Then pass the key's local file to the installer:
`echo-setup install --config bridge.toml --openai-api-key-file openai-key.txt` (add `--force` to replace an installed
ChatGPT config; `update` accepts the option too). The installer refuses the option with a config whose mode is
`chatgpt`. It drops one trailing newline and refuses a blank key, a key with leading or trailing spaces, and any
character outside visible ASCII. It streams the key to the Echo over adb stdin, never on a command line, and writes
it as `live.credential_file` owned by the bridge user with mode 0600. It then skips ChatGPT sign-in, checks that
`echo-bridge status` reports `mode: api-key` and
continues with Port OS. `echo-setup status --json` and `doctor` report `auth_mode` (`api_key`, `chatgpt` or
`unavailable`), never the key.

To replace the key later, open **Settings > Accounts > OpenAI (GPT Live)** on the Echo screen. In `api_key` mode it
shows whether a key is set, and a **Save key** field in place of Link and Remove. The bridge applies the same rule
as the installer, keeps the replaced key beside the new one as `<live.credential_file>.previous` (mode 0600), and
restarts at the next settled boundary so the app server logs in with the new key. Neither the screen nor any
response, log or trace shows the key. OpenAI first checks a key when it is used, so a
wrong key may still show as set; it surfaces as a failed voice call or delegated turn (and as "not accepted" when
the app server refuses the login). To go back, copy the `.previous` file over the key with the
bridge stopped. The bridge does not start without a well-formed key file, so Settings can replace a key but not
recover a missing one: run the installer with `--openai-api-key-file` instead.

Keep `live.credential_file` in a private location outside both app-server homes. ChatGPT keeps its
sign-in in `<codex.home>/auth.json`. For example, with `codex.home = "/srv/echo/private/codex-home"`,
API-key mode uses `/srv/echo/private/codex-home-api-key`. Each home keeps its own model cache, logs
database and working directory.

To switch modes, stop the bridge, change `live.mode`, and start it again. On ChatGPT startup the bridge
removes the app-server copy of the API key from the sibling's `auth.json`, if present. The nominated key
file stays available for a later API-key start. Both modes refuse startup if `live.credential_file` refers
to the sibling's `auth.json`, directly or through a filesystem alias. Move the nominated key to its own
private file and update the setting before retrying. An unsafe or busy sibling, an auth file that cannot
be safely removed, or a credential path collision reports `codex.home: private ownership unavailable`.
If the filesystem cannot represent the derived sibling name, ChatGPT can still use its existing home.
Choose a shorter home name before using API-key mode.

Existing ChatGPT sign-ins work in place. If an older API-key trial already replaced the ChatGPT auth file,
stop the bridge, select `chatgpt`, and run `echo-bridge signin --config PATH` to restore the sign-in. Before
rolling back to an older bridge, select `chatgpt` to avoid that older binary writing API auth to the shared home.

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
| `live.mode` | `chatgpt` (default) or `api_key`. `api_key` skips ChatGPT sign-in; usage is billed to the key at API rates. | restart / no |
| `live.credential_file` | Required literal path; current ChatGPT backend instead uses Codex home; API-key backend reads this file (one line, 0600). `install --openai-api-key-file` and Settings **Save key** write it. | restart / no |
| `live.voice` | Optional legacy voice fallback when codex.voice absent; pinned voice list below. | restart / no |
| `live.session_idle_seconds` | Unsigned 64-bit seconds, default 120; Live-call reuse idle timeout, not post-reply listening. | restart / no |
| `voice` | Optional run-listening table, defaults below. | restart / no |
| `voice.session_listen_enabled` | Boolean, default true; keep listening after reply. | boundary / yes |
| `voice.session_listen_seconds` | Integer 3–300, default 15; listening window. Must not exceed session_max_seconds. | boundary / yes |
| `voice.session_max_seconds` | Integer 3–2147483647, default 600; run ceiling for delegated waiting and agent-granted listening extensions, at least listen seconds. Active speech and playback finish under their existing rules. | restart / no |
| `voice.speech_start_expiry_seconds` | Integer 3–300, default 20; how long a user speech start with no transcript text counts as speech. The pinned V3 session sends no speech-stopped, so without text the latch expires and the no-answer, idle and listen deadlines apply again. A speech start after a reply that expires this way took no turn: the run listens one `voice.session_listen_seconds` from the expiry, then ends. Speech also holds the `voice.session_max_seconds` cap by this long at most. | restart / no |
| `voice.local_barge_cut` | Boolean, default true. True: a sign of the user talking over a reply holds the reply and, once the model yields, cuts it and drops the audio queued for the Echo, once per user turn; the rest of that turn's speech cuts nothing, so the model's backchannels play. False: the bridge never holds or cuts on user speech; the Live model's own yield is the only barge-in, and the Echo plays out the audio already queued (about 0.4–0.6 s). Trace: `barge.cut`, `barge.suppressed_in_turn`, `barge.rearmed`, and `barge.model_only` when false. | restart / no |
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
| `portos_endpoint.gateway_url` | Optional WSS template ending in `/devices/{authLinkId}`. Defaults to the Port OS device gateway when the API base is the Port OS API. | restart / no |
| `portos_endpoint.api_base` | Optional HTTPS API base. Defaults to the configured Port OS grant's HTTPS resource origin. | restart / no |
| `camera` | Optional camera table for snapshots from the native Echo screen app. | see individual keys |
| `camera.enabled` | Boolean, default true on Android and false on Linux. Linux always reports the production camera as unavailable. | restart / no |
| `camera.id` | Exact Android Camera2 ID, default `"0"`. Use a nonblank string without control characters. | restart / no |
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
| `codex.home` | Default unset; absolute private directory, required at runtime, mode exactly 0700. API-key mode uses a sibling named with the `-api-key` suffix. | restart / no |
| `codex.binary` | Default unset (pinned host helper resolves multi-tool); if set absolute path. Device must name installed standalone app server. | restart / no |
| `codex.launch` | `subcommand` default (`app-server --listen stdio://`) or `standalone` (`--listen stdio://`); standalone requires binary. | restart / no |
| `codex.ca_file` | Default unset; absolute PEM path, child SSL_CERT_FILE and bridge Port OS OAuth/MCP additional trust roots. | restart / no |
| `codex.web_search` | `live` default (also with table/key omitted), `cached` or `disabled`. Provider-backed search only (hosted or native `web.run`); all other tool restrictions stay intact. Invalid type/value: `codex.web_search: invalid type or value`, exit 2 before runtime effects. Set disabled and restart to turn off; remove key before restoring an older binary. | boundary / yes |
| `codex.voice` | Default unset; overrides live.voice. `default` omits outgoing field; other choices below, nonblank. | boundary / yes |
| `codex.model` | Optional nonblank legacy runner-model alias; overrides agent.model. | restart / no |
| `codex.delegate_model` | Optional nonblank runner model; overrides both older model keys. Unset everywhere, the bridge selects `gpt-6.1-sol`. An empty screen override selects that default even when a legacy model key is set. gpt-6.2-* models currently refuse the direct `web.run` that search needs (HTTP 400, decision 2026-11-21), so every delegated turn fails with them while search is on. | boundary / yes |
| `codex.delegate_effort` | Optional nonempty string sent to runner; decoder imposes no enum, pinned app server validates supported effort. | boundary / yes |
| `codex.delegate_speed` | `standard` default, `fast` (priority) or `ultrafast`. Ultrafast needs `gpt-6-astra` or `gpt-6.1-sol` and costs more; an unsupported tier is served as standard and traced as `call.service_tier_downgraded`. Invalid type/value: `codex.delegate_speed: invalid type or value`. | boundary / yes |
| `codex.delegate_fast` | Legacy boolean read when `delegate_speed` is absent: true is `fast`, false is `standard`. Both set to different tiers: `codex.delegate_speed: conflicts with codex.delegate_fast; remove delegate_fast`. Settings no longer writes it. | boundary / yes |
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
voice that is not listed. A configured voice absent from the app server's usable list is left out of the call,
with one line on stderr, and the app server's default voice speaks. The v2-only voices earlier builds offered (`alloy`,
`ash`, `ballad`, `cedar`, `coral`, `echo`, `sage`, `shimmer`, `verse`) still load from an existing `bridge.toml` or
`settings.toml`, as `default` with one settings warning; they are never offered or saved.
Offline, only the shape of a voice name is checked: lowercase ASCII letters, at most 32.
`live.voice` is a legacy fallback; use `codex.voice` for the screen editor.

In `private/bridge.trace`, `call.realtime_start_sent` records `voice` as the name sent in
`thread/realtime/start`, or `"omitted"` when the request leaves the choice to the server. `voice_listed`
is `true` for a named match, `false` for a miss or default selection with a usable V3 list, and `null`
when no usable V3 list was returned. When a usable list rejects a named voice, `call.voice_dropped`
records `configured_voice` and `listed_count` before the start marker. These events describe the request
that left the bridge.

## Camera snapshots

To select a specific Android Camera2 device, use the following base configuration.

```toml
[camera]
enabled = true
id = "rear camera"
```

To disable snapshots, set `enabled = false` in `[camera]` and restart the bridge.
The screen page stays available. Remove `[camera]` before reverting to a binary
that predates this option.

Upgrade the screen APK with the same signing key to preserve its data. Setup
grants CAMERA before launch. Reprovision the screen once through the authorized
installer's existing `--url` flow with the paired numeric loopback URL to enable
the native exchange. The display can continue using its saved URL until then.

The bridge admits a single on-demand JPEG only with a fresh open shutter
observation. A closed shutter returns `SHUTTERED`. A missing or unreliable
switch returns `CAMERA_ERROR`. Microphone mute allows capture. The exchange
holds the image in memory and the full request expires after three seconds.

## Checking changes

Run `make lint-docs` on a provisioned Linux host. Its coverage reader extracts full accepted paths from
production decoder modules, not the stale secret-diagnostic inventory. Missing canonical rows fail with
`docs/configuration.md: undocumented config keys: <sorted full paths>`.
Unsupported decoder syntax or missing source fails closed, requiring a checker update rather than silently
certifying partial coverage. The dedicated `scripts/test_lint_docs.py` self-tests exercise omission/restoration,
new accepted keys and unsupported input without building or running Echo.
