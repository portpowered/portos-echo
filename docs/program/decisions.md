---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; decision log. Newest first within each list
---

# Decisions

Every owner request and decision lands here in the change that acts on it (`docs/program/rules.md`). The
coordinator records its own calls here too, marked "coordinator". Open questions say what is recommended and who
decides.

## Decided

| Date | Decision | By | Where it applies |
|---|---|---|---|
| 2026-11-15 | One coordinator agent builds the repository on a simplified runtime: a Claude Code session plus one-shot `you` dispatches. No project leads, integration lead or lane owners. The drafts' `el-f5-lane-ownership` gate is dropped | Owner | `docs/program/coordinator.md`, plan section 10 |
| 2026-11-15 | The wake word is the one echo-dot-assist uses: Pryon "Alexa" from the firmware's `libpryon` | Owner | Plan section 5 |
| 2026-11-15 | The operator sets up GPT-Live access. The code follows OpenClaw's implementation of ChatGPT sign-in with Codex's OAuth client, and documents Codex's token exchange to a Platform API key as the alternative | Owner | Plan section 4 |
| 2026-11-15 | The gateway's default credential mode is `chatgpt` (OpenClaw's direct path, `gpt-live-1-codex`); `api-key` is the fallback with no change on the Echo | Coordinator, following the owner's OpenClaw direction | Plan section 4.1 |
| 2026-11-15 | The gateway is a Rust binary in this workspace (`echo-gateway`) sharing `echo-contracts`, held to the same CI gates | Coordinator | Plan section 6 |
| 2026-11-15 | Public repository, Apache License 2.0 | Owner | `LICENSE`, `docs/program/rules.md` |
| 2026-11-13 | DPoP binds the Port OS refresh token only. MCP requests use Bearer; no DPoP nonce or `ath` on MCP | Owner | Plan sections 8.3 (M5) and 9 |
| 2026-11-13 | Every MCP invocation runs on the Echo; the server side (the gateway) handles the models. Client delegation only, never `responses` delegation | Owner | Plan section 3 |
| 2026-11-13 | Muse is reached through a wrapper around the Muse gadget SDK, with one callback command for correlation | Owner | Plan section 7 |
| 2026-11-13 | The bench drives a Ring camera through the Port OS CLI; `echo-lab` never carries its own Ring peer | Owner | Plan section 8.4 |
| 2026-11-13 | CI gates for tests, coverage and lint are required checks from Phase F on; this repository is never merged with `--admin` | Owner | Plan section 11 |

## Open

| Question | Recommendation | Decides |
|---|---|---|
| Where does the gateway run? | The operator's Windows PC on the LAN first; a Raspberry Pi (which can also host the Muse bridge) once it must be always on. A cloud VM only if the Echo leaves the LAN. Moving it means moving the operator's ChatGPT credential | Owner |
| Which model serves the delegated agent, and what are its caps? | Start with the default Codex model that the `chatgpt` mode lists; at most 8 tool calls and 60 s per task; a daily request cap in the gateway config | Owner |
| Does the operator accept the `chatgpt` mode's risks (terms, entitlement, preview interface; plan section 4.5)? | Yes for development and the bench; revisit before anyone else uses the device | Owner |
