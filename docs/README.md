---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: active; documentation index
---

# Documentation

[Documentation index](README.md)

## Program

- [Program plan](program/plan.md)
- [Standing rules](program/rules.md)
- [Coordinator and factory](program/coordinator.md)
- [Measured baselines](program/baselines.md)
- [Decisions](program/decisions.md)
- [Required checks and branch protection](program/branch-protection.md)

## Architecture decisions

- [Port OS OAuth client dependencies](adr/0010-mcp-auth.md)
- [Provisional Live call adapter](adr/0011-live-call.md)
- [Provisional Responses agent loop](adr/0013-echo-agent-loop.md)
- [Independent ChatGPT credential source](adr/0015-chatgpt-auth.md)
- [S1 WebRTC transport](adr/0009-live-transport.md)

- [Runtime-free delegated-task seam](adr/0007-echo-agent-api.md)
- [PCM resampling and native Opus](adr/0008-echo-audio.md)
- [Shared bridge contracts](adr/0006-echo-contracts.md)
- [Bounded Wyoming client](adr/0012-echo-wyoming.md)
- [ADR guide](adr/README.md)
- [Workspace harness dependencies](adr/0000-workspace-baseline.md)
- [TOML lanes declarations](adr/0001-xtask-toml.md)
- [Local milestone schema validation](adr/0003-echo-lab-schema-validation.md)
- [Bridge TOML configuration](adr/0005-echo-config.md)
- [Private Cargo metadata parsing](adr/0002-xtask-parsing-deps.md)

- [Checked screen messages](adr/0004-screen-contract.md)

## Spikes

- [S2 Wyoming reply cancellation](spikes/s2-barge-in.md)

## Original drafts

- [Execution plan](design/echo-live-execution-plan.md)
- [RFC](design/echo-live-rfc.md)

## Checking documentation

Provision the pinned tools before running offline validation:

~~~sh
npx --yes markdownlint-cli2@0.20.0 --version
npx --yes @mermaid-js/mermaid-cli@11.12.0 --version
~~~

Install missing lychee with `cargo install --locked --version 0.24.2 lychee`, as permitted by the Phase F packet.
Use Python 3.12 (`python` on Windows, `python3` on Ubuntu), Node 24, GNU Make and POSIX sh.
The Mermaid CLI requires its supported local headless browser; a missing browser fails the gate.

~~~sh
make lint-docs
make lint
~~~

The default gate checks local links and anchors without external requests after provisioning.
`make lint-docs-online` also checks external links and is reserved for explicit scheduled validation.
Both modes compile every actual Mermaid block in tracked Markdown.
Add each new tracked documentation page to this index in the same change.

- [Stateless MCP HTTP transport](adr/0014-mcp-http.md)
- [Bridge-hosted delegation runner](adr/0017-echo-delegation.md)
- [Loopback screen transport](adr/0016-screen-server.md)
- [Operator sign-in command library](adr/0018-signin.md)
