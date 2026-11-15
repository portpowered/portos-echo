---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# 0004: Shared bridge contracts

## Context

Bridge components need one pure vocabulary before transports and execution.
Wyoming captures from hassmic pin 8646a4f ground the named event tags. The
capture format retains merged JSON and payload digests, not original section
placement. Replay reconstructs only the capture script's silence and constant
PCM samples, rejecting any other payload identity.

## Decision

Use serde, serde_json, thiserror and schemars for validated string identifiers,
raw framing, typed messages and deterministic JSON Schema. Keep Secret free of
Debug, Display and Serialize; ordinary field drop makes no zeroization claim.
Use trybuild compiler negatives and proptest for framing properties, as dev-only
dependencies. Compiler-negative inputs use .txt files compiled by trybuild: they
are intentionally non-executable fixtures, not coverage-bearing Rust targets.
Their transitive packages below support compiler diagnostics,
TOML metadata, random generation and platform entropy. No transport, network,
async runtime or hardware dependency belongs in the library.

Detection, transcript, voice-stopped and error Wyoming arms are explicitly
requested but uncaptured and retain opaque data. Live shapes are PROVISIONAL
until M2: only delegation-created, commentary-append, session-created and error
are recognized. Unknown values pass through; malformed known data fails.
Representing the responses delegation target does not authorize executing it.

Schemas compare without writes by default. UPDATE_SCHEMAS=1 explicitly updates
them; changes require contract review. Revert this PR as a unit before consumers
land; subsequent compatibility changes belong in coordinator contract PRs.

## Packages covered

- `serde`
- `serde_json`
- `thiserror`
- `schemars`
- `chacha20`
- `core_detect`
- `cpufeatures`
- `echo-contracts`
- `getrandom`
- `glob`
- `proptest`
- `r-efi`
- `rand`
- `rand_core`
- `rand_xorshift`
- `target-tuple`
- `termcolor`
- `toml`
- `toml_datetime`
- `trybuild`
- `unarray`
- `winapi-util`
- `windows-sys`
