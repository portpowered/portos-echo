---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: provisional; standalone library, bridge mounting and real provider proof pending
---

# 0017: Bridge-hosted delegation runner

## Decision

Add `echo-delegation`, a standalone library without an echo-bridge dependency.
Reuse one ResponsesBackend with one tracing McpClient ToolHost. The caller constructs
the MCP client from its selected McpServer and injected TokenProvider, and supplies
ModelCredential and TraceSink adapters. Existing config controls model selection,
tool caps, task deadlines and the backend's daily request budget; no defaults or
credential storage are introduced here. No external package is added to Cargo.lock.

A client delegation becomes a Task with explicit host instructions and the complete
serialized LiveEvent as context, including opaque fields. Responses-target events
are refused before observation or submission; other events have no effect. Completed
and failed terminal text becomes one same-ID commentary with empty extra fields.
Submission and missing-terminal failures return fixed error categories.

TraceSink is synchronous observation only. Hosts must keep it bounded and must not
block or expose secrets. Its three events observe accepted delegation, ToolHost
dispatch attempt and prepared commentary, respectively. A denied tool still has
one dispatch trace but no MCP request. The host owns actual Live delivery and trace
persistence. This crate owns no Live socket and never retries a task or tool.

## Lifecycle and proof

Reuse the runner for sequential delegations so daily accounting is retained. Drive
each run to terminal; when abandoning a run future, drop the entire runner to stop
its owned backend workers. An already received MCP effect cannot be undone. This
API does not promise safe runner reuse after abandonment or durable accounting.

T1 tests exercise mapping, refusal, terminal text and fixed failures with in-memory
fakes. The T3 delegation_t3 suite uses real ResponsesBackend and McpClient transports,
echo-spoof-mcp, and FakeLive on ephemeral loopback. It covers D01-D12 including exact
trace order, arguments, model continuation, configured boundaries, reusable budgets,
UTF-8 isolation, timeout and abandonment. Readiness signals precede fake-clock
advances. Synthetic Responses fixtures follow the existing agent-loop fixture
protocol. No real provider, credential, account or hardware is used.

Run `cargo test --offline --locked -p echo-delegation -- --nocapture` and `make ci`.
Review owns R-BEHAVIOR, R-CI, R-MERGE and V-LIBRARY. W2B-BRIDGE-WIRE owns mounting,
socket delivery policy and trace persistence; W2-M4-SIM owns the full simulated
milestone; M2-REAL and M4-M5-REAL own separately authorized real-edge proof.

Rollback removes this unmounted crate, its lane registration and this ADR/index
entry, then regenerates Cargo.lock. Shared contracts and config remain unchanged.
