.DEFAULT_GOAL := ci
SHELL := sh
.SHELLFLAGS := -ec

.PHONY: fmt-check lint-shape lint lint-rust test-unit test-contract test-sim build-host ci lint-rules lint-scripts-web
.NOTPARALLEL: lint ci

fmt-check:
	cargo fmt --all --check

lint-shape:
	cargo xtask lint-shape

lint: fmt-check lint-rust lint-deps lint-rules lint-shape lint-scripts-web

lint-rust:
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: lint-deps
lint-deps:
	cargo xtask lint-deps && cargo deny --offline check

lint-rules:
	cargo xtask lint-rules && gitleaks dir . --no-banner --redact

lint-scripts-web:
	sh scripts/lint-scripts-web.sh

test-unit:
	cargo test --workspace --all-targets

test-contract:
	printf '%s\n' 'no contract tests yet'

test-sim:
	printf '%s\n' 'no scenarios yet'

build-host:
	cargo build --workspace --locked

ci: lint test-unit test-contract test-sim build-host
