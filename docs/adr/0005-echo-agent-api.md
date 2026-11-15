---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# 0005: Runtime-free delegated-task seam

## Context

Bridge consumers need a shared task boundary before production execution exists.
The W0 seam is additive and reuses echo-contracts identifiers and task events.

## Decision

Use ordinary trait methods returning borrowing futures with a Send guarantee,
as recorded in the coordinator's W0 decision. No runtime or async-trait is needed.
Use serde, serde_json and thiserror for values and errors. Feature-gated fakes
match complete inputs, record attempts and return in-memory scripts. They carry
deadlines and tool caps unchanged; enforcing them belongs to the executor.

Use pollster 0.4 as a dev-only executor for deterministic behavioral tests.
It adds no transitive dependencies and never enters the normal dependency tree.
The local echo-agent-api package is the other new lock entry. Existing shared
dependencies remain covered by the shared-contracts ADR. Revert the new crate
and its registration together before downstream consumers land.

## Packages covered

- `echo-agent-api`
- `pollster`

The tool seam preserves opaque JSON and separates a successful tool error result
(`is_error = true`) from a boundary `ToolError`. FakeToolHost matches the full
name/arguments/idempotency-key triple, records unsuccessful attempts too, and
consumes the first matching script only. Repeated keys consume successive
scripts; the fake is not a durable idempotency or authorization mechanism.
Generic tests exercise all five methods with Send futures under pollster.
The assembled witness uses explicitly scripted task events, rather than
inferring completion from a tool call. Real MCP and executor behavior belong
to the downstream W1/W2 lanes.
