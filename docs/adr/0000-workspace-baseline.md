---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Workspace harness dependencies

## Status

Accepted.

## Context

Phase F establishes the repository harness before product crates.

## Decision

Record the packages already present when the rule gate is introduced.
The workspace task runner uses only the Rust standard library.
This ADR grants no exception to checking subsequently added package names.

## Packages covered

- `xtask`
