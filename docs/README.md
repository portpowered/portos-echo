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
- [Correlated Muse core](adr/0020-muse-core.md)
- [Authenticated Muse service](adr/0022-muse-service.md)
- [Maintained WebView screen client](adr/0023-echo-screen.md)
- [Bridge audio orchestration](adr/0024-echo-bridge.md)

- [Protocol-neutral Echo link](adr/0025-echo-link.md)
- [Pinned Codex Android ARM build](adr/0028-codex-armv7.md)
- [Pinned Codex app-server client](adr/0026-codex-client.md)
- [Codex local fake: S5](spikes/codex-local-fake.md)
- [Frame-based Codex media peer](adr/0027-echo-media.md)

## Windows WSL helpers

Windows helpers select one guest with `ECHO_WSL_DISTRO`. Unset or empty selects
Ubuntu; a nonempty value must contain only ASCII letters, digits, `.`, `_` or `-`.
Names preserve case. Invalid input fails before WSL runs and names the variable.
WSL failures name the resolved distro on stderr; there is no fallback or install.
Linux uses native tools and ignores this setting, including invalid values.

Select the provisioned replacement from PowerShell:

~~~powershell
$env:ECHO_WSL_DISTRO = 'pe-ci'
~~~

For POSIX shells use `export ECHO_WSL_DISTRO=pe-ci`. Never probe the damaged
default guest. Unset/empty refusal tests use only the deny-only WSL substitute
in `python scripts/test_build_echo.py -v`; they prove selection and error
propagation, not the damaged guest's behavior.

Every build/test using pe-ci holds `/var/tmp/pe-ci.lock` without a timeout.
First verify interop (this starts only the selected replacement):

~~~powershell
wsl -d pe-ci -u root -- flock /var/tmp/pe-ci.lock `
  /mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile -Command "Write-Output WSL_INTEROP_OK"
~~~

Require exit 0 and `WSL_INTEROP_OK`; stop and report if unavailable. Save this
native validation script outside the checkout, replacing the worktree path:

~~~powershell
Set-Location '<worktree>'
$env:ECHO_WSL_DISTRO = 'pe-ci'
$env:PATH = (python scripts/make-posix.py) + ';' + $env:PATH
sh tests/hassmic_host_smoke.sh
if ($LASTEXITCODE) { exit $LASTEXITCODE }
python scripts/test_build_echo.py -v
exit $LASTEXITCODE
~~~

Run that script under the same lock, using its absolute Windows path:

~~~powershell
wsl -d pe-ci -u root -- flock /var/tmp/pe-ci.lock `
  /mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile -File '<validation.ps1>'
~~~

Do not nest lock acquisition inside the Windows script. The bridge refusal
helper `sh crates/echo-bridge/tests/bridge_refusal/host.sh` uses the same setting:
retain its stdin, require a numeric port on stdout, close stdin, require
`cleaned` and exit 0, and collect the launcher. Use prebuilt native echo-lab
binaries for the W1 sim scenario; the runner never builds them.
For Linux quality, source `/opt/pe-ci-env.sh` from a guest script, change to the
worktree, and run `make ci` while holding the same lock. Long runs launch with
`setsid -f`; print `QUALITY_LOCK_ACQUIRED` after acquiring the lock and capture
the actual `QUALITY_EXIT` status alongside the tested HEAD. Keep logs outside git.
