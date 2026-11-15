.DEFAULT_GOAL := ci
SHELL := sh
.SHELLFLAGS := -ec

.PHONY: fmt-check lint test-unit test-contract test-sim build-host ci
.NOTPARALLEL: lint ci

fmt-check:
	cargo fmt --all --check

lint: fmt-check

test-unit:
	cargo test --workspace --all-targets

test-contract:
	printf '%s\n' 'no contract tests yet'

test-sim:
	printf '%s\n' 'no scenarios yet'

build-host:
	cargo build --workspace --locked

ci: lint test-unit test-contract test-sim build-host
