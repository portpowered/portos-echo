---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# 0001: Parse lanes declarations with toml

`cargo xtask lint-shape` compares Cargo workspace members with `lanes.toml`.
Use the task-authorized `toml = "0.9"` dependency to parse the declaration
document, reject malformed input and preserve TOML string semantics. Handwritten
line matching would misread valid declarations and fail to reject invalid ones.

This is the only new direct dependency. Cargo generated the lockfile using
Rust 1.100.0. Newly resolved registry packages are:

- `toml` 0.9.12+spec-1.1.0
- `toml_datetime` 0.7.5+spec-1.1.0
- `toml_parser` 1.1.5+spec-1.1.0
- `toml_writer` 1.2.0+spec-1.1.0
- `serde_core` 1.0.229
- `serde_derive` 1.0.229
- `serde_spanned` 1.1.3
- `indexmap` 2.14.2
- `equivalent` 1.0.3
- `hashbrown` 0.17.1
- `proc-macro2` 1.0.107
- `quote` 1.0.47
- `syn` 3.0.10
- `unicode-ident` 1.0.27
- `winnow` 0.7.15 and 1.0.5

These are TOML's transitive parsing, serialization, ordered-map and derive
dependencies, including optional packages recorded by Cargo's resolver.
The tool is build-time only and introduces no product networking or runtime.
Cargo metadata JSON uses a private standard-library parser; no JSON dependency
is added. Remove the direct dependency and regenerate the lockfile to reverse
this decision if the lint implementation is reverted.
