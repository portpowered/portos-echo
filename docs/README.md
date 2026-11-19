---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 18
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

- [Architecture and conversation lifecycle](architecture.md)
- [Five-target touch calibration](adr/0019-touch-calibration.md)

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
- [S3 The screen client](spikes/s3-screen-client.md)
- [The Codex app server as the voice backend](spikes/codex-app-server.md)

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
Use Python 3.12 (`python` and `python3` on PATH), Node 24, GNU Make and POSIX sh on Linux.
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
- [Correlated Muse core](adr/0020-muse-core.md)
- [Authenticated Muse service](adr/0022-muse-service.md)
- [Maintained WebView screen client](adr/0023-echo-screen.md)
- [Bridge audio orchestration](adr/0024-echo-bridge.md)

- [Protocol-neutral Echo link](adr/0025-echo-link.md)
- [Pinned Codex Android ARM build](adr/0028-codex-armv7.md)
- [Pinned Codex app-server client](adr/0026-codex-client.md)
- [Native installer orchestration](adr/0029-echo-setup.md)
- [Codex local fake: S5](spikes/codex-local-fake.md)
- [Frame-based Codex media peer](adr/0027-echo-media.md)

## Linux hosts

Linux is the only supported platform for building, testing and running Echo.
Use the provisioned tools in `.github/actions/setup/action.yml`; helpers never
install a toolchain implicitly. Build once with `make hassmic-host` and
`cargo build -p echo-lab -p echo-bridge --bins --locked`, then run the documented
`echo-lab run <scenario> --driver sim` entry points with local fake providers.
The host controller launches `hassmic-host` directly as a Linux process.

Hosted CI is the quality gate. Use `targeted.yml` for focused Linux witnesses;
never queue a full local `make ci`. The supported local Linux host is pe-ci.
Authorized local builds/tests source `/opt/pe-ci-env.sh`, hold
`/var/tmp/pe-ci.lock`, and use a private `/var/tmp` target removed on exit.
Use relative worktree gitdir links when sharing a checkout with the host OS.

Coordinator device runs also execute from Linux pe-ci with explicit Linux
`adb` and `port` paths and the existing forwarding, firmware, quiet-hours and
session safeguards. No networking or device configuration is changed by the
platform removal. Physical reachability and acoustic proof remain coordinator
T5 work; workers use explicit local fake adb/port binaries only.
