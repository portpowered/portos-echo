---
author: Port OS Team
owner: Port OS Team
last modified: 2026, november, 15
status: owner action requested after the CI pull request merges
---

# Required checks and branch protection

After this change merges, the owner must protect `main` with pull requests required,
these exact GitHub Actions check names required, and branches up to date before merge:

- `format`
- `lint-rust`
- `lint-shape`
- `lint-deps`
- `lint-rules`
- `lint-scripts-web`
- `lint-docs`
- `lint-ci-parity`
- `lint-self-tests`
- `test-unit`
- `test-contract`
- `test-sim`
- `test-concurrency`
- `coverage`
- `build-host`

Require the GitHub Actions app as the status source. Apply protections to administrators,
with no bypass actors, force pushes or branch deletion. Enable squash merging only;
disable merge commits, rebase merges and auto-merge. Never bypass a red required check.
Review verifies the observed check names against this list before owner application.
The owner proves that a red required check prevents merging after protection is applied.
This document requests settings; it does not apply them.

Each job in `.github/workflows/ci.yml` invokes one corresponding Make target.
`make ci` executes all 15 targets in the same order, enforced by `cargo xtask ci-parity`.
`make lint` also runs parity and both existing lint self-tests.
Local execution needs the tools pinned in `.github/actions/setup/action.yml`, GNU Make,
POSIX sh, Python 3.12, Node 24 and the stable compiler in `rust-toolchain.toml`.
Nextest runs each unit test once, with zero retries and a 30-second timeout.

Coverage alone uses the approved nightly compiler:

~~~sh
rustup toolchain install nightly-2026-11-05 --profile minimal --component llvm-tools-preview
cargo install --locked --version 0.9.1 cargo-llvm-cov
make coverage COVERAGE_BASE=origin/main
~~~

An explicit base commit may replace `origin/main`. The report is written to
`target/llvm-cov/coverage.lcov`. Coverage checks workspace line and branch floors,
the higher named-crate floors when those crates exist, and changed executable Rust lines.
Missing or malformed evidence fails. No generated-source exclusions are currently needed.
The report disables the tool's default filename filter, including test source evidence.
The stable compiler remains the default for every other gate.

The scheduled workflow runs online documentation validation nightly and advisory refresh
weekly, and permits manual dispatch. Cloud, mutation and upstream-host jobs remain deferred
until their documented prerequisites exist. Concurrency currently reports
`no concurrency tests yet`; no product concurrency paths exist to exercise.

Hosted seed failures, final green checks, independent clean-room validation, squash merge,
owner settings application and integrated M0 validation remain later gates owned by review.
The echo-lab M0 command checks harness readiness only; it does not establish M0 acceptance.
