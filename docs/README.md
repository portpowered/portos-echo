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

- [Runtime-free delegated-task seam](adr/0005-echo-agent-api.md)
- [Shared bridge contracts](adr/0004-echo-contracts.md)
- [ADR guide](adr/README.md)
- [Workspace harness dependencies](adr/0000-workspace-baseline.md)
- [TOML lanes declarations](adr/0001-xtask-toml.md)
- [Local milestone schema validation](adr/0002-echo-lab-schema-validation.md)
- [Bridge TOML configuration](adr/0003-echo-config.md)
- [Private Cargo metadata parsing](adr/el-f4-xtask-parsing-deps.md)

- [Checked screen messages](adr/0003-screen-contract.md)

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
