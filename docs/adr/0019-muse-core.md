---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: accepted
---

# Correlated Muse core

`echo-muse-bridge` implements the existing `echo-agent-api` backend boundary
without mounting a backend into the bridge. Each admitted task gets an independent
UUID side chat and an absolute deadline. The pure message composer requests
`echo.report.result`; conversational text becomes progress, even when the gadget
claims success. Strict intake accepts exactly task_id, status and summary and
acknowledges only a committed matching live result. A report never grants consent
or approval. The dotted command name follows the SDK naming rule recorded in
the [program plan](../program/plan.md#7-muse-a-wrapper-around-the-gadget-sdk).

Process execution, epoch time and persistence are separate traits. `CliGadget`
uses argv with closed stdin, validates wait seconds before spawning, preserves
exit 0/3 output, and maps trust-store and other process failures without retrying.
The core sends with wait=1 and does not poll chat. A pending event consumer waits
for a store notification or the injected deadline. Tests advance an injected
clock and use readiness barriers, never elapsed wall time.

Store transactions clone, validate and commit before publishing state. JSON
snapshots use version 1, atomic same-directory replacement and Unix mode 0600.
The caller provides a private directory outside git. One store owns mutations;
intake borrows that owner. Sequential reopening is supported, independent
concurrent writers are not. Consumed events stay consumed across restarts;
terminal tombstones remain. Invalid snapshots fail closed without migration or
pruning. Reopening never replays a send.

Store locks never cross an asynchronous wait. A separate per-task send lock
serializes initial and cancellation requests within the chat. Cancellation
commits and clears its queue before the advisory send, and remains cancelled
when that send fails. Report, deadline and late chat replies recheck the stored
state so a terminal task cannot be resurrected. Admission errors map to the
existing `UnscriptedTask`; gadget errors after admission queue a nonretryable
failure with safe text. The event API has no error variant: a store failure
returns no event without consuming the stored queue. The host must treat a
storage fault as requiring recovery, rather than proof of a successful task.

Dependencies are existing locked `serde`, `serde_json`, `thiserror`, `getrandom`,
`uuid`, `tempfile`, and `tokio`, plus `echo-agent-api` and `echo-contracts`.
UUID bytes come from `getrandom` and `uuid::Builder`; no new UUID entropy feature
or external package is needed. Binding operator Answer A (2026-11-16 02:40 UTC)
authorizes only this crate's registration and Cargo-generated lock changes in
addition to the crate and this ADR/index entry.

T1 lifecycle tests exercise exact outcomes, task isolation, failure rollback,
pending consumers and both terminal transition orders. T2 verifies strict reports
and byte-equal command JSON. Component T3 invokes the production adapter against
a Cargo-prebuilt Rust fake with per-case scripts/captures, and exercises an
assembled submit/process/report/result flow. These prove no actual Muse, handler
IPC, authenticated LAN transport, bridge mount, or M7 milestone behavior.
Those belong to SERVICE-AUTH, BRIDGE-MUSE and M7-T4/M7-T5. Hosted CI, coverage,
independent validation and merge remain review gates.

Rollback removes the additive crate, its registration and docs, and regenerates
the lockfile. It leaves caller-owned persisted data intact. This change installs
nothing, exposes no socket and changes no active profile or tool executor.
