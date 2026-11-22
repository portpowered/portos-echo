---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 21
status: active; accepted technical debt
---

# Technical debt

This file records known debt that we accept for now. Each item has its own section with the owner decision date, the
problem, why we accept it now, the exit plan and the trigger to act. When an item is paid off, its section says so and
links the change that removed it.

## Delegated agent depends on Codex code mode and backend tool reservations (2026-11-21)

**Owner decision date.** 2026-11-21. Owner: "switch to 6.1 and offer directly without code-mode for now".
Recorded in [decisions](../program/decisions.md) and
[ADR 0026](../adr/0026-codex-client.md#search-restored-by-a-direct-web-declaration-2026-11-21).

**Problem.**

- Every model in the Codex catalog is `code_mode_only`. A tool that is not declared directly sits behind
  `functions.exec`, which runs in `codex-code-mode-host` (V8). That host is not built for the Echo's armv7.
- The backend reserves `web.run` on gpt-6.2-* models. It answers a client declaration that does not match its own
  schema with HTTP 400.
- So the Echo is pinned to `gpt-6.1-sol` as the default delegated model, with `web` and `functions` declared
  directly (`features.code_mode.direct_only_tool_namespaces`).
- Codex releases and backend changes that we do not control have broken the Echo twice: the HTTP 400 that PR 323
  worked around, and then search vanishing without an error.

**Why we accept it now.** Codex provides the ChatGPT sign-in, the Live realtime call (`thread/realtime/start`), the
voice list and the delegated tool loop.

**Exit plan (owner, 2026-11-21).** Remove the Codex dependency. The bridge would call the API endpoints that Codex
calls for us today: the sign-in and token refresh, the realtime call, the responses and tool loop, and search. After
that, no Codex binary and no code mode run on the Echo.

- Alternative: build `codex-code-mode-host` for armv7. This is heavy because of V8.

**Trigger to act.** Any one of these: `gpt-6.1-sol` is retired, `gpt-6.1-sol` starts reserving `web.run`, or another
Codex release breaks the Echo.
