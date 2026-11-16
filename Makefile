.DEFAULT_GOAL := ci
ifeq ($(OS),Windows_NT)
SHELL := cmd.exe
.SHELLFLAGS := /d /s /c
POSIX_BIN := $(shell python scripts/make-posix.py)
ifneq ($(.SHELLSTATUS),0)
$(error POSIX bootstrap failed; see tool and path diagnostics above)
endif
export PATH := $(POSIX_BIN);$(PATH)
SHELL := $(POSIX_BIN)/sh.exe
else
SHELL := sh
endif
.SHELLFLAGS := -ec
COVERAGE_BASE ?= origin/main
# Cargo uses response files for long Windows compiler commands. The coverage
# wrapper cannot read their crate names; direct instrumentation preserves mappings.
ifeq ($(OS),Windows_NT)
COVERAGE_INSTRUMENTATION := --no-rustc-wrapper
endif

.PHONY: fmt-check lint-shape lint lint-rust lint-deps lint-rules lint-scripts-web lint-docs lint-docs-online lint-ci-parity lint-self-tests test-unit test-contract test-sim test-concurrency coverage deps-fresh build-host ci
.NOTPARALLEL: lint ci

fmt-check:
	cargo fmt --all --check
	git ls-files -z -- '*.sh' ':(exclude)factory/' | xargs -0 shfmt -d -i 2 -ci --
	if [ -f web/screen/package.json ]; then cd web/screen && npm ci && npx --offline prettier --check .; fi

lint-shape:
	cargo xtask lint-shape

lint: fmt-check lint-rust lint-deps lint-rules lint-shape lint-scripts-web lint-docs lint-ci-parity lint-self-tests

lint-rust:
	cargo clippy --workspace --all-targets --locked -- -D warnings

lint-deps:
	cargo xtask lint-deps && cargo deny --offline check

lint-rules:
	cargo xtask lint-rules && python scripts/lint_secrets.py

lint-scripts-web:
	sh scripts/lint-scripts-web.sh

lint-docs:
	sh scripts/lint-docs.sh

lint-docs-online:
	sh scripts/lint-docs.sh --online

lint-ci-parity:
	cargo xtask ci-parity

lint-self-tests:
	sh scripts/test-lint-scripts-web.sh
	python scripts/test_lint_docs.py
	python scripts/test_make_posix.py
	python scripts/test_coverage_profiles.py

test-unit:
	cargo nextest run --workspace --all-targets --locked --config-file .github/nextest.toml --profile ci
	python -m unittest discover -s crates/echo-lab/src/lab_run/sim/host_tests
	if [ -f web/screen/package.json ]; then cd web/screen && npm ci && npm test; fi

test-contract:
	cargo test --offline --locked -p echo-contracts --lib --test contracts_wyoming --test contracts_events -- --nocapture
	cargo test --offline --locked -p echo-screen-contract --test screen_wire -- --nocapture

test-sim:
	cargo run --locked -p echo-lab -- validate
	$(MAKE) hassmic-host
	sh tests/hassmic_host_smoke.sh
	cargo nextest run -p echo-wyoming --test wyoming_t3 --run-ignored only --locked --config-file .github/nextest.toml --profile ci --no-capture
	cargo nextest run -p echo-lab --test sim_bridge_cli --run-ignored only --locked --config-file .github/nextest.toml --profile ci --no-capture
	cargo build -p echo-lab --bins --locked
	cargo nextest run -p echo-lab --test sim_host --run-ignored only --locked --config-file .github/nextest.toml --profile ci --test-threads 2 --success-output immediate
	cargo nextest run -p echo-lab --bin echo-lab --run-ignored only --locked --config-file .github/nextest.toml --profile ci --test-threads 2 --success-output immediate
	cargo run --locked -p echo-lab -- run --all --driver sim

.PHONY: hassmic-host capture-wyoming measure-barge-in
hassmic-host:
	sh scripts/hassmic-host.sh build

capture-wyoming: hassmic-host
	sh tests/hassmic_host_smoke.sh --capture

measure-barge-in: hassmic-host
	sh tests/hassmic_host_smoke.sh --measure-barge-in

test-concurrency:
	cargo test -p echo-audio --lib --locked loom -- --nocapture
	cargo test -p echo-wyoming --lib --locked loom_queue -- --nocapture

# Sources outside the workspace that LLVM still attributes lines to: dependency macro expansions (tokio's
# join!/select!) from the cargo registry or git checkouts, and the toolchain's own library. The default
# ignore regex stays off so that every workspace path, tests/ included, is counted. Any other outside path
# still fails coverage-check.
COVERAGE_DEPENDENCY_SOURCES := \.cargo.(registry|git).|\.rustup.toolchains.|.rustc.[0-9a-f]{40}.
COVERAGE_HOST_T3 := -p echo-wyoming --test wyoming_t3

coverage: hassmic-host
	cargo +nightly-2026-11-05 llvm-cov clean --workspace
	mkdir -p target/llvm-cov
	cargo +nightly-2026-11-05 llvm-cov $(COVERAGE_INSTRUMENTATION) --workspace --all-targets --locked --branch --no-report
	cargo +nightly-2026-11-05 llvm-cov $(COVERAGE_INSTRUMENTATION) $(COVERAGE_HOST_T3) --locked --branch --no-report -- --ignored --nocapture
	cargo +nightly-2026-11-05 llvm-cov $(COVERAGE_INSTRUMENTATION) -p echo-lab --test sim_bridge_cli --locked --branch --no-report -- --ignored --nocapture
	cargo +nightly-2026-11-05 llvm-cov $(COVERAGE_INSTRUMENTATION) -p echo-lab --test sim_host --locked --branch --no-report -- --ignored --nocapture --test-threads=2
	cargo +nightly-2026-11-05 llvm-cov $(COVERAGE_INSTRUMENTATION) -p echo-lab --bin echo-lab --locked --branch --no-report -- --ignored --nocapture
	python scripts/coverage_profiles.py
	cargo +nightly-2026-11-05 llvm-cov report --branch --no-default-ignore-filename-regex --ignore-filename-regex '$(COVERAGE_DEPENDENCY_SOURCES)' --fail-under-lines 80 --lcov --output-path target/llvm-cov/coverage.lcov
	cargo xtask coverage-check target/llvm-cov/coverage.lcov "$(COVERAGE_BASE)"

deps-fresh:
	cargo deny fetch && cargo deny check

build-host:
	cargo build --workspace --locked
	git diff --exit-code HEAD -- Cargo.lock
	$(MAKE) check-echo-screen

.PHONY: build-echo-screen test-echo-screen-jvm test-echo-screen-url
build-echo-screen:
	sh scripts/build-echo-screen.sh

test-echo-screen-jvm:
	sh tests/echo_screen_jvm.sh

test-echo-screen-url:
	cargo test --offline --locked -p echo-screen-server --lib page_url -- --nocapture

.PHONY: test-echo-screen-install check-echo-screen
test-echo-screen-install:
	cargo build --offline --locked -p echo-lab --bin echo-lab-fake-adb
	python tests/test_echo_screen_install.py

check-echo-screen: build-echo-screen test-echo-screen-jvm test-echo-screen-url test-echo-screen-install
	python tests/verify_echo_screen_apk.py

ci: fmt-check lint-rust lint-shape lint-deps lint-rules lint-scripts-web lint-docs lint-ci-parity lint-self-tests test-unit test-contract test-sim test-concurrency coverage build-host
