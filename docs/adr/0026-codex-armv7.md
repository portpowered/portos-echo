---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 16
status: proposed; Android build evidence pending
---

# Pinned Codex Android ARM build

The owner's W2d decision authorizes building OpenAI Codex release
`rust-v0.160.0`, commit `a956835d020762cb2b570053af06f643a11c0ecc`,
for the future W4 deployment. This target builds its standalone
`codex-app-server`, without changing repository crates or runtime behavior.
The [research brief](../spikes/codex-app-server.md) describes an older main;
the release pin wins. At this pin `codex-rs/rust-toolchain.toml` says
1.95.0, rather than the payload's 1.99.1. We explicitly select installed
Rust 1.100.0 and NDK 27.3.13750724, API 24.

The release manifest versions its workspace packages as 0.160.0 but its
lockfile retains 159 local packages at 0.0.0. Patch 0001 reconciles only
those local lock versions so Cargo can run with `--locked`. It changes no
external version, checksum, feature or dependency. Its zero-context hunks
apply only after verifying the exact pinned source. Other build failures
remain blockers; behavioral portability changes require a new decision.

Coordinator mailbox answer 1 (2026-11-16) authorizes patch 0003 for locked
`pagable` 0.4.1 only: ARM32's layout is ten words, while the existing
non-ARM32 (including wasm32) assertion retains twelve. Neither assertion is
removed and no runtime code changes. The coordinator checked that no runtime
code depends on this size. The helper downloads the crate outside git, checks
the checksum from the pinned lockfile, and applies the patch there. Cargo's
`paths` override selects that private copy without changing the lockfile,
version, dependencies or shared registry cache. GNU tar is also required to
unpack the crate.

Coordinator mailbox answer 2 (2026-11-16) authorizes patch 0004: in
`exec-server/src/no_follow/unix.rs`, lines 220/221/228/229 and 313/314/320,
widen libc's `S_IFMT`, `S_IFLNK`, `S_IFDIR` and `S_IFREG` constants from
ARM32 Android's `u16` to rustix `Stat.st_mode`'s `u32` using `u32::from`.
Every mask, comparison, symlink rejection and removal decision is preserved.
The same answer permits only identical lossless integer-width fixes elsewhere
in the pinned closure, each documented in its patch header. Narrowing, casts
that could truncate, branches, cfgs, features, dependencies and other behavior
changes still require a new decision. Patch 0004 also compiles on hosts where
these constants already are `u32`.

The pre-approved upstream closure includes OpenSSL, aws-lc, portable-pty,
bundled SQLite and compression libraries. No package enters this repository's
Cargo.lock. pe-ci has no Android static `OPENSSL_DIR`; `openssl-sys` 0.9.111
fails because pkg-config cannot cross-compile. Patch 0002 enables its existing
vendored feature for ARMv7 Android only, as explicitly authorized. The locked
`openssl-src` supplies the source. NDK tools are on PATH and
`ANDROID_NDK_ROOT` selects the NDK for OpenSSL's Android configuration.
Dynamic OpenSSL and every SONAME outside the explicit Android system list
are rejected. No TLS validation bypass is allowed.

## Operator procedure

Provision Rust 1.100.0 with target `armv7-linux-androideabi`, the Linux NDK,
Git, curl, cmake, make, Perl, pkg-config, Python 3, file and GNU coreutils.
Run in a native Linux worktree. On pe-ci source `/opt/pe-ci-env.sh` first.
Select distinct new or empty absolute directories outside the repository,
with existing parents:

~~~sh
export CODEX_ARMV7_WORK_DIR=/var/tmp/codex-arm-build-1
export CODEX_ARMV7_OUTPUT_DIR=/var/tmp/codex-arm-output-1
python3 third_party/codex/test_build.py
make build-codex-armv7
~~~

The target runs at niceness 10 with exactly four Cargo workers and its own
target directory, without the quality lock. Source is shallow-fetched;
configuration patches apply in lexical order. Staging and directory claims
isolate invocations. A complete verified bundle is renamed into place on
success. Failure leaves source/build diagnostics in the work directory and
removes only invocation-owned staging and empty claim directories. Existing
nonempty output is refused and preserved. Do not reuse a failed work directory:
select another clean pair. SIGKILL cannot run cleanup; inspect and remove only
the abandoned invocation's claim directories before attempting reuse.

The bundle contains stripped `codex-app-server`, `cacert.pem`, upstream
`LICENSE` and complete `NOTICE` (including OpenAI Codex, Copyright 2025 OpenAI
and upstream attributions). Codex is Apache-2.0. The CA bundle retains its
Mozilla/MPL-2.0 attribution and is downloaded from
`https://curl.se/ca/cacert-2026-11-05.pem`, verified against
SHA-256 `427c064216761a1e72a8b30f0d954d08cb6e4435a82091fb865afd20c8497285`.
Updating either pin requires review. No downloaded checksum is trusted.

For the coordinator's future deployment, place the bundle under
`/data/local/echo/codex/` and configure
`CODEX_CA_CERTIFICATE=/data/local/echo/codex/cacert.pem` and
`SSL_CERT_FILE=/data/local/echo/codex/cacert.pem`. At the pin,
`CODEX_CA_CERTIFICATE` takes precedence over nonempty `SSL_CERT_FILE`.
This lane does not install or execute Android code, contact providers,
or prove on-device TLS, memory use or account compatibility.

The dedicated dispatch/weekly workflow uploads the verified bundle. The
large upstream build stays outside `make ci`. Repeat the procedure with a
second clean directory pair and post both byte counts and hashes in the PR;
equal size is required, while hash reproducibility is reported honestly.
Review owns hosted workflow execution, terminal CI, merge, clean-room checks,
W2d integration and the coordinator's T5 execution/RSS measurement.
Rollback removes this opt-in target, workflow and helper; external outputs
remain operator-owned.
