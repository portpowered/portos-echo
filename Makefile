.DEFAULT_GOAL := ci
SHELL := sh
.SHELLFLAGS := -ec
COVERAGE_BASE ?= origin/main

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
	cargo xtask lint-rules && gitleaks dir . --no-banner --redact

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

test-unit:
	cargo nextest run --workspace --all-targets --locked --config-file .github/nextest.toml --profile ci

test-contract:
	printf '%s\n' 'no contract tests yet'

test-sim:
	cargo run --locked -p echo-lab -- validate
	$(MAKE) hassmic-host
	sh tests/hassmic_host_smoke.sh

.PHONY: hassmic-host capture-wyoming
hassmic-host:
	sh scripts/hassmic-host.sh build

capture-wyoming: hassmic-host
	sh tests/hassmic_host_smoke.sh --capture

test-concurrency:
	printf '%s\n' 'no concurrency tests yet'

coverage:
	cargo +nightly-2026-11-05 llvm-cov clean --workspace
	mkdir -p target/llvm-cov
	cargo +nightly-2026-11-05 llvm-cov --workspace --all-targets --locked --branch --no-default-ignore-filename-regex --fail-under-lines 80 --lcov --output-path target/llvm-cov/coverage.lcov
	cargo xtask coverage-check target/llvm-cov/coverage.lcov "$(COVERAGE_BASE)"

deps-fresh:
	cargo deny fetch && cargo deny check

build-host:
	cargo build --workspace --locked
	git diff --exit-code HEAD -- Cargo.lock

ci: fmt-check lint-rust lint-shape lint-deps lint-rules lint-scripts-web lint-docs lint-ci-parity lint-self-tests test-unit test-contract test-sim test-concurrency coverage build-host
