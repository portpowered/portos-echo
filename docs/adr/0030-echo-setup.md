---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 19
status: accepted
---

# Native installer orchestration

The owner's admitted echo-setup mission authorizes a native host installer for
Windows x86_64 and Linux x86_64. The platform exception applies to this package
only. The bridge and all other Echo packages remain Linux-only.

`echo-setup` is a first-party workspace package using the already locked
`reqwest`, `serde`, `serde_json`, `sha2` and `tokio` packages. No third-party
package or version is added. HTTPS release downloads use the existing native
root certificate support. Explicit literal loopback mirrors support controlled
tests. Redirects are inspected before following them, including downgrade
refusal. Integrity checks precede adb discovery.

The native executable invokes adb through native argv and never invokes a host
shell. Device identity is an opaque argv element. Dependency output stays behind
fixed diagnostics. The installer embeds the authored shell as a trusted archive
staging adapter; after member/type validation the release bundle's own shell
owns lifecycle operations. Direct-device and Linux host transports use the same
shell lifecycle code. The private launch path and FIFO/control/stop/finished
protocol stay compatible with the fork's boot consumer.

Cargo builds the native recording adb fixture from tests/support; release builds
select `--bin echo-setup` explicitly. Functional tests invoke the native CLI with
private per-case directories and loopback release servers. A Linux witness runs
the actual shell against controlled Android paths and tools. These tests do not
prove device tool availability, provider authentication or acoustic wake; those
remain coordinator T5 outcomes. Release integration consumes built artifacts.

Rollback of this architectural choice removes the host package and restores the
manual Linux installer entry point, which remains supported throughout migration.
