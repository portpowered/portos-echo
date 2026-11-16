---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# 0002: Private Cargo metadata parsing

The binding F4 decision authorizes serde with derive and serde_json and their
resolution closure for xtask only. Typed JSON decoding replaces fragile text
matching. The decoder stays private; no product dependency or public API is added.
Cargo generated Cargo.lock. Every newly resolved package is listed below.

| Package | Version | License expression | Reason |
| --- | --- | --- | --- |
| serde | 1.0.229 | MIT OR Apache-2.0 | Typed metadata deserialization |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | Serde deserialization traits |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 | Derive private metadata decoders |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | Decode Cargo JSON |
| itoa | 1.0.19 | MIT OR Apache-2.0 | JSON integer formatting |
| memchr | 2.8.3 | Unlicense OR MIT | JSON byte scanning; select MIT |
| zmij | 1.0.23 | MIT | JSON numeric conversion |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | Derive token processing |
| quote | 1.0.47 | MIT OR Apache-2.0 | Derive code generation |
| syn | 3.0.10 | MIT OR Apache-2.0 | Derive syntax parsing |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | Identifier classification |

All packages come from crates.io. The policy permits a compliant branch of every
alternative expression and both required parts of unicode-ident's expression.
No advisory ignore, baseline, allowlist or suppression is added.

The checker reads one offline locked metadata snapshot. Its single rule table
checks declared normal/build edges, including optional and target-specific edges,
and excludes dev-only declarations. No transitive rule is introduced.

Provision cargo-deny with `cargo install --locked cargo-deny` if absent; prepare
registry and advisory caches outside test execution. The Make target runs all
four policy checks offline. Rollback removes the checker, policy, Make target
and parsing dependencies together, then regenerates Cargo.lock using Cargo.
