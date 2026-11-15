# Factory testing standards

---
author: Port OS Team
last modified: 2026, november, 15
doc-id: FSTD-005
---

This standard is the test-layer authority for factory planning, implementation,
review, and validation in portos-echo. It sits under the repository's own rules:
`docs/program/rules.md` (Tests) and `docs/program/plan.md` section 8 (test tiers,
scenario files, milestones). Where this document and those disagree, they win.

## Quick rules

- Choose the lowest tier that can prove the behavior without crossing a boundary
  that tier does not own.
- **Behavior, not presence.** Assert values: the call count is 1, the delegation
  id matches, the spoken reply contains the phrase. `len(x) > 0` and "no error"
  are not acceptance.
- Tiers T0–T2 never touch the network. T3 uses loopback and local fakes only. T4
  (real services) runs only when the PRD explicitly authorizes it. T5 and T6
  (the Echo Show, the Ring bench, soak) are operator-run and never run by a
  factory worker.
- One scenario file per milestone or failure case, run by every driver
  (`--driver sim|cloud|device`); only the driver changes, never the oracle.
- Time is a fake clock in T1–T3. No fixed sleeps; timeouts are ceilings and
  readiness is signal-driven.
- Coverage only goes up. A change that lowers a crate's line coverage fails.
- Repository shape, dependency direction, inventories and naming belong in lint
  or static checks (`cargo xtask lint-*`), never in runtime tests.
- A test that cannot name the behavior and observer it protects is removed,
  rewritten as behavioral proof, or moved to a static check.

## 1. Classify by behavior and boundary

| Tier | Proves | Allowed boundary | Must not become |
| --- | --- | --- | --- |
| T0 static | Formatting, lints, shape, layering, schema snapshots, secrets | Source and metadata | A runtime test |
| T1 unit and property | One component, function or state machine | Direct calls with explicit in-memory fakes; proptest; loom on concurrency paths | A cross-crate journey or anything opening a socket |
| T2 contract | A published schema, wire format or captured trace round-trips | Fixtures and captured traces | A substitute for runtime behavior |
| T3 simulated system | A customer-observable flow end to end | Real binaries and crates against local fakes (spoof MCP, mock OAuth, fake Wyoming peer) on loopback, fake clock | A test that needs the internet, hardware or real credentials |
| T4 cloud integration | The same scenario against real services | Real GPT-Live and the Port OS pilot with simulator devices and a test principal | A broad matrix; anything run without explicit authorization |

## 2. Location and naming (Rust workspace)

- Unit tests live beside the code in `#[cfg(test)]` modules.
- Cross-module tests of one crate live in `crates/<crate>/tests/`.
- Scenario files live in `tests/scenarios/<id>.yaml` and are run by `echo-lab`.
- Network-touching tests carry the `cloud` tag; hardware tests the `hardware`
  tag. Untagged tests that open a non-loopback socket fail T0.

## 3. Required planning record

For every added or changed test, the planner records:

1. the behavior being proved;
2. the tier and why a lower tier cannot prove it;
3. the boundary used, and the real versus faked dependencies;
4. the isolation model (fresh temp dirs, ports, fake clock);
5. the exact command or scenario and the values it must print.

## 4. Implementation and review enforcement

Implementers run focused tests locally and the repository's quality command
(`make ci` when a `Makefile` exists, otherwise the PRD's acceptance command).
They never weaken an assertion or add an `#[allow]`, baseline or lowered
threshold to get green (`docs/program/rules.md`).

Reviewers reject:

- tests that assert presence instead of values;
- unit tests that assemble a whole system, and system tests without an
  observable assertion;
- fixed-sleep synchronization, shared mutable fixtures or hidden serialization;
- any test that touches the network outside its tier, or hardware at all;
- secrets, serial numbers or personal data in fixtures.

Review evidence names the behavior proved, the tier, the command run and its
output, and any edge that remains unproven.

## 5. Delivery checklist

- Every test has one named behavior and observer.
- T0–T2 tests have no network; T3 is loopback only.
- Scenarios run through `echo-lab` with the same file for every driver.
- No fixed sleeps; the fake clock drives time.
- The quality command passes locally; hosted CI passes once it exists.
