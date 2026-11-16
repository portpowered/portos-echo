---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# 0004: Checked screen messages

## Context

The bridge and page need one independent data boundary before implementation.
This library has no network, runtime, hardware or authentication dependency.

## Decision

Use serde with derive and serde_json for internally tagged snake_case messages,
and schemars 1 for JSON Schema generation from those same public types.
Keep exact wire fixtures and pretty schema snapshots (including a final newline).
Normal tests only compare; maintainers explicitly set UPDATE_SCHEMAS=1 to update.
Review wire compatibility whenever updating snapshots. Unknown object fields are
ignored and unknown tags rejected. Strings have no added validation; unsigned
numbers retain the Rust u64 range. ScreenFrame nests its tagged state.
Transport, pairing, action correlation, rendering and JavaScript integer handling
remain with W1. This publishes no server or UI and introduces no deployment.

## Packages covered

- `echo-screen-contract`
- `serde`
- `serde_derive`
- `serde_json`
- `schemars`
- `schemars_derive`
- `serde_derive_internals`
- `dyn-clone`
