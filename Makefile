.DEFAULT_GOAL := ci
SHELL := sh
.SHELLFLAGS := -ec

.PHONY: fmt-check lint-shape lint lint-rust test-unit test-contract test-sim build-host ci lint-rules
.NOTPARALLEL: lint ci

fmt-check:
	cargo fmt --all --check

lint-shape:
	cargo xtask lint-shape

lint: fmt-check lint-rust lint-rules lint-shape

lint-rust:
	cargo clippy --workspace --all-targets --locked -- -D warnings

lint-rules:
	cargo xtask lint-rules && gitleaks dir . --no-banner --redact

test-unit:
	cargo test --workspace --all-targets

test-contract:
	printf '%s\n' 'no contract tests yet'

test-sim:
	printf '%s\n' 'no scenarios yet'

build-host:
	cargo build --workspace --locked

ci: lint test-unit test-contract test-sim build-host
