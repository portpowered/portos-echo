---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: accepted
---

# 0002: Local milestone schema validation

## Status

Accepted for the Phase F echo-lab harness.

## Context

Operators need structural scenario validation and an honest static M0 inventory
before drivers exist. JSON Schema Draft 2020-12 and YAML parsing are required by
the F9 packet. No runtime, network or hardware dependency is needed.

## Decision

Use serde and serde_json for JSON values, serde_norway for YAML, and jsonschema
0.33 with default features disabled. The validator embeds authored schemas and
uses only internal references; HTTP and file resolvers are disabled. Keep the
small command parser in the binary instead of adding clap. Inherit workspace
lints and the unchanged dependency license/advisory policy.

The transitive closure supports schema references, regular expressions, numeric
validation, Unicode/URI formats, serialization and platform abstractions. Some
packages are target-specific; their inclusion grants no network or device use.
Cargo generates the lockfile. Future dependency changes remain subject to the
same ADR and cargo-deny gates.

## Packages covered

- `ahash`
- `aho-corasick`
- `autocfg`
- `base64`
- `bit-set`
- `bit-vec`
- `bitflags`
- `borrow-or-share`
- `bumpalo`
- `bytecount`
- `cfg-if`
- `displaydoc`
- `echo-lab`
- `email_address`
- `fancy-regex`
- `fluent-uri`
- `fraction`
- `futures-core`
- `futures-task`
- `futures-util`
- `getrandom`
- `icu_collections`
- `icu_locale_core`
- `icu_normalizer`
- `icu_normalizer_data`
- `icu_properties`
- `icu_properties_data`
- `icu_provider`
- `idna`
- `idna_adapter`
- `js-sys`
- `jsonschema`
- `lazy_static`
- `libc`
- `litemap`
- `lock_api`
- `num`
- `num-bigint`
- `num-cmp`
- `num-complex`
- `num-integer`
- `num-iter`
- `num-rational`
- `num-traits`
- `once_cell`
- `outref`
- `parking_lot`
- `parking_lot_core`
- `percent-encoding`
- `pin-project-lite`
- `potential_utf`
- `r-efi`
- `redox_syscall`
- `ref-cast`
- `ref-cast-impl`
- `referencing`
- `regex`
- `regex-automata`
- `regex-syntax`
- `rustversion`
- `ryu`
- `scopeguard`
- `serde_norway`
- `slab`
- `smallvec`
- `stable_deref_trait`
- `synstructure`
- `tinystr`
- `unsafe-libyaml-norway`
- `utf8_iter`
- `utf_types`
- `uuid`
- `uuid-simd`
- `version_check`
- `vsimd`
- `wasip2`
- `wasm-bindgen`
- `wasm-bindgen-macro`
- `wasm-bindgen-macro-support`
- `wasm-bindgen-shared`
- `windows-link`
- `wit-bindgen`
- `writeable`
- `yoke`
- `yoke-derive`
- `zerocopy`
- `zerocopy-derive`
- `zerofrom`
- `zerofrom-derive`
- `zerotrie`
- `zerovec`
- `zerovec-derive`
