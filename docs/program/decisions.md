---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; decision log. Newest first within each list
---

# Decisions

Every owner request and decision lands here in the change that acts on it (`docs/program/rules.md`). The
coordinator records its own calls here too, marked "coordinator". Open questions say what is recommended and who
decides. A decision that a later one replaces stays in the table, marked "superseded", so the history reads in
one place.

## Decided

| Date | Decision | By | Where it applies |
|---|---|---|---|
| 2026-11-15 | **The hassmic `-D` route.** The Echo runs echo-dot-assist's `hassmic` in Wyoming mode with `-D` (duplex for a realtime client, echo-dot-assist #173): Pryon wake word, Amazon's echo canceller, microphone and speaker, mute latch. Everything else moves to `echo-bridge` on the operator's PC: the Wyoming client, the GPT-Live session, the delegated agent loop and every MCP call. A small screen client stays on the Echo for status, prompts and M6. No Rust, C shim or WebRTC peer of ours on the Echo | Owner | Plan sections 1–3, 6, 8.3, 10 |
| 2026-11-15 | **MCP runs on the bridge**, on the owner's side and never at OpenAI (client delegation only, never `responses` delegation). The Port OS credential lives only on the bridge host. Supersedes the 2026-11-13 decision that every MCP invocation runs on the Echo | Owner | Plan section 3 |
| 2026-11-15 | **The work is split by repository.** portos-echo owns the plan, the bridge, hassmic integration, the screen, `echo-lab` and the bench, CI, Muse and milestones M0, B0, M1–M7. portos-backend owns plan section 9: `/mcp` accepts any valid Port OS access token for the MCP resource the same way whatever grant minted it, with nothing specific to device login | Owner | Plan section 9 |
| 2026-11-15 | **Work runs on the you-agent-factory**, adapted from its baseline into `factory/` and proven on sample work items before program work is submitted. Every agent worker is Codex with the `gpt-6.1-sol` model (processor, reviewer, validator and the rest); the planner may use `gpt-6-astra`. Supersedes the same day's "one coordinator on one-shot `you` dispatches" runtime: the coordinator stays the single operator and integrator, the factory does the lanes | Owner | `docs/program/coordinator.md`, `factory/` |
| 2026-11-15 | The bridge is a Rust binary in this workspace (`echo-bridge`, formerly `echo-gateway`) sharing `echo-contracts`, held to the same CI gates | Coordinator | Plan section 6 |
| 2026-11-15 | The wake word is the one echo-dot-assist uses: Pryon "Alexa" from the firmware's `libpryon` | Owner | Plan section 5 |
| 2026-11-15 | The operator sets up GPT-Live access. The code follows OpenClaw's implementation of ChatGPT sign-in with Codex's OAuth client, and documents Codex's token exchange to a Platform API key as the alternative | Owner | Plan section 4 |
| 2026-11-15 | The bridge's default credential mode is `chatgpt` (OpenClaw's direct path, `gpt-live-1-codex`); `api-key` is the fallback with no change on the Echo | Coordinator, following the owner's OpenClaw direction | Plan section 4.1 |
| 2026-11-15 | Public repository, Apache License 2.0 | Owner | `LICENSE`, `docs/program/rules.md` |
| 2026-11-15 | ~~One coordinator agent builds the repository on a simplified runtime: a Claude Code session plus one-shot `you` dispatches~~ Superseded the same day by the factory decision above. The drafts' `el-f5-lane-ownership` gate stays dropped | Owner | `docs/program/coordinator.md` |
| 2026-11-13 | DPoP binds the Port OS refresh token only. MCP requests use Bearer; no DPoP nonce or `ath` on MCP | Owner | Plan sections 8.3 (M5) and 9 |
| 2026-11-13 | ~~Every MCP invocation runs on the Echo; the server side (the gateway) handles the models~~ Superseded 2026-11-15: MCP runs on the bridge. Client delegation only, never `responses` delegation, still holds | Owner | Plan section 3 |
| 2026-11-13 | Muse is reached through a wrapper around the Muse gadget SDK, with one callback command for correlation | Owner | Plan section 7 |
| 2026-11-13 | The bench drives a Ring camera through the Port OS CLI; `echo-lab` never carries its own Ring peer | Owner | Plan section 8.4 |
| 2026-11-13 | CI gates for tests, coverage and lint are required checks from Phase F on; this repository is never merged with `--admin` | Owner | Plan section 11 |

## Open

| Question | Recommendation | Decides |
|---|---|---|
| May hassmic be installed on the Echo (build 7809)? `install-system.sh` writes `/system`, takes Amazon's apps off the partition and extends the SELinux policy; `--uninstall` and the firmware `.bin` in TWRP undo it. 7809 differs from the pinned 7080 in the same two libraries as the tested 7806 (`docs/program/baselines.md`) | Yes, once the bridge's T3 run against `hassmic-host` passes, so the device is changed only when there is something to connect | Owner |
| Which wake word? The firmware carries Pryon models for "Alexa", "Amazon", "Computer" and "Echo" (2026-11-15 probe), and hassmic supports all of them | Keep "Alexa" (decided). Consider "Computer" for the bench only, so stock Echos in the house don't answer the bench's utterances | Owner |
| Where does the bridge run? | The operator's Windows PC on the LAN first (in WSL, beside the CI-identical toolchain); a Raspberry Pi (which can also host the Muse bridge) once it must be always on. Moving it means moving the operator's ChatGPT credential and the Port OS grant | Owner |
| Which model serves the delegated agent, and what are its caps? | Start with the default Codex model that the `chatgpt` mode lists; at most 8 tool calls and 60 s per task; a daily request cap in the bridge config | Owner |
| Does the operator accept the `chatgpt` mode's risks (terms, entitlement, preview interface; plan section 4.5)? | Yes for development and the bench; revisit before anyone else uses the device | Owner |
| Which Port OS client does the bridge use for the device-code grant? | The built-in public CLI client for development and T4 (it already allows `device_code` and `refresh_token`). A dedicated `echo` client (a management registration, no code change) before anyone else uses the device, so its grants show and revoke separately from the CLI's | Owner |
| How does the screen client run? | A page served by the bridge, shown full screen by a minimal WebView app of our own (`echo-screen`) set as hassmic's home app. Kiosk Satellite already runs this way on athena 7806, which shows WebView apps work after the install. Spike S3 (plan section 3.1) decides it | Coordinator, after spike S3 |
