---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: accepted
---

# ADR 0022: One authenticated Muse service owner

Extend the existing Muse crate with opt-in Linux `echo-muse-daemon` and
`echo-muse-report` targets. One leased durable backend handles both HTTPS task
operations and private Unix callbacks. The handler forwards stdin and validates
the committed accepted identity; it has no task store or independent backend.
This extends [plan section 7](../program/plan.md#7-muse-a-wrapper-around-the-gadget-sdk)
without changing shared contracts, the command spec, snapshots or bridge wiring.
Reports are results, never consent. Installation and real SDK/M7 proof remain
later gates.

Reuse locked `axum`, `hyper`, `hyper-util`, `rustls`, `tokio-rustls`, `subtle`
and the existing `tokio` features for bounded HTTP/1.1, TLS, constant-time client
credential comparison, signals, IPC and child execution. `axum` supplies bounded
body collection; `hyper` owns protocol parsing and one-request connections.
TLS uses an explicitly selected ring provider, private DER key/certificate and
operator-provisioned trust. There is no plaintext listener or default address.
No new external package or version is added to Cargo.lock.

Test-only `rcgen` (with its already locked `aws-lc-rs` crypto), `getrandom` and
`reqwest` generate disposable trust and credentials and verify actual TLS
connections. Unix-only test dependencies avoid requiring the certificate
generator's native assembler on Windows, where portable regressions and explicit
unsupported-executable behavior remain enabled. No real service is contacted.

The daemon holds a standard-library OS file lease on the canonical store
companion inode through connection drain and child reaping. Store health is
latched after any transaction failure, independently of the existing Option
event API. Quotas preserve rather than prune tombstones. Atomic replacement
syncs the file and parent directory. Event removal commits before response:
disconnects have uncertain receipt and consumption is at most once, with no
automatic retries. These semantics are explicit in the crate API contract.

All transports and outputs are bounded; child execution uses fixed argv with
closed stdin and kill/wait on timeout or output overflow. Private callbacks
require full framed input and half-close before mutation. Authentication runs
before routing/lookup/body dispatch. Configuration rejects implicit listeners,
unsafe private files, symlinks and uncontrolled test-clock access.

Component T3 tests use Cargo-prebuilt production binaries and fake gadget,
loopback HTTPS, private sockets and temporary filesystem state. They directly
observe exact events, handler bytes/exits, call counts, restart/no replay,
ownership, cancellation, deadlines and failure bounds. Pure parser/quota/race
branches remain T1/T2. This is not a milestone scenario or actual SDK proof.
Independent review, hosted coverage/terminal CI, clean-room validation and merge
remain review-owned gates. Rollback stops the opt-in daemon and reverts the
additive service, preserving operator state and lock inode.
