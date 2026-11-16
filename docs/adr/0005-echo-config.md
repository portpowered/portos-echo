---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# Bridge TOML configuration dependencies

## Status

Accepted for el-w0-config.

## Context

The bridge needs one synchronous configuration loader with field-specific diagnostics.
Credentials stay in files; this library retains their paths and reads only the TOML.

## Decision

Use the existing serde and toml families for typed field decoding and TOML parsing,
and thiserror for an opaque safe error. Parser errors and their source snippets are
discarded. Explicit field decoding supplies trusted diagnostic context without
exposing rejected values. No URL, network, runtime or hardware dependency is added.

The library validates HTTP(S) authority syntax offline and never resolves hosts.
Unknown fields are rejected at their enclosing known section. Credential paths are
literal, with no environment overrides or existence checks. The example and runtime
tests protect this behavior; bridge credential loading belongs to W2.

## Packages covered

- echo-config
- thiserror
- thiserror-impl

serde and toml and their existing transitive packages were already in the lockfile.
