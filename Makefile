.DEFAULT_GOAL := ci
SHELL := sh
.SHELLFLAGS := -ec
COVERAGE_BASE ?= origin/main

.PHONY: fmt-check lint-shape lint lint-rust lint-deps lint-rules lint-scripts-web lint-docs lint-docs-online lint-ci-parity lint-self-tests test-unit test-contract test-sim test-sim-suites test-sim-host test-concurrency coverage deps-fresh build-host ci
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
	python scripts/test_coverage_profiles.py
	python scripts/test_build_echo.py

# CI runs each nextest invocation below as parallel shards, one per runner, by passing
# NEXTEST_PARTITION=count:K/N; the shards together run every test. Unset, a target runs all of them.
NEXTEST_PARTITION ?=
NEXTEST_SHARD = $(if $(NEXTEST_PARTITION),--partition $(NEXTEST_PARTITION) --no-tests=pass)

.PHONY: test-unit-rust test-unit-python test-unit-screen
test-unit: test-unit-rust test-unit-python test-unit-screen

test-unit-rust:
	cargo nextest run --workspace --all-targets --locked --config-file .github/nextest.toml --profile ci $(NEXTEST_SHARD)

test-unit-python:
	python -m unittest discover -s crates/echo-lab/src/lab_run/sim/host_tests
	python -m unittest discover -s crates/echo-lab/src/lab_run/device/clip_tests

test-unit-screen:
	if [ -f web/screen/package.json ]; then cd web/screen && npm ci && npm test; fi

.PHONY: codex-app-server
codex-app-server:
	sh scripts/codex-app-server.sh

test-unit-rust coverage: codex-app-server

test-contract:
	cargo test --offline --locked -p echo-contracts --lib --test contracts_wyoming --test contracts_events -- --nocapture
	cargo test --offline --locked -p echo-screen-contract --test screen_wire -- --nocapture

# CI runs the two halves as parallel jobs; `make test-sim` still runs both.
test-sim: test-sim-suites test-sim-host

.PHONY: sim-prereqs
sim-prereqs: hassmic-host
	cargo build --locked -p echo-bridge -p echo-muse-bridge --bins
	cargo build --locked -p echo-lab --bins

# CI runs the three parts of test-sim-suites as parallel jobs; `make test-sim-suites` runs all three.
.PHONY: test-sim-scenarios test-sim-bridge test-sim-components
test-sim-suites: test-sim-scenarios test-sim-bridge test-sim-components

test-sim-scenarios: sim-prereqs
	cargo run --locked -p echo-lab -- validate
	python -c "import resource,subprocess,sys,time; t=time.time(); r=subprocess.run(['cargo','run','--locked','-p','echo-lab','--','run','--all','--driver','sim']); u=resource.getrusage(resource.RUSAGE_CHILDREN); print(f'TIMING wall={time.time()-t:.1f} user={u.ru_utime:.1f} sys={u.ru_stime:.1f}'); sys.exit(r.returncode)"

test-sim-bridge: sim-prereqs
	cargo nextest run -p echo-bridge --test bridge_t3 --run-ignored only --locked --config-file .github/nextest.toml --profile ci --success-output immediate

test-sim-components: sim-prereqs
	sh tests/hassmic_host_smoke.sh
	cargo nextest run -p echo-wyoming --test wyoming_t3 --run-ignored only --locked --config-file .github/nextest.toml --profile ci --success-output immediate
	cargo nextest run -p echo-lab --test sim_bridge_cli --run-ignored only --locked --config-file .github/nextest.toml --profile ci --success-output immediate

test-sim-host: sim-prereqs
	cargo nextest run -p echo-lab --test sim_host --run-ignored only --locked --config-file .github/nextest.toml --profile ci --test-threads 2 --success-output immediate $(NEXTEST_SHARD)
	cargo nextest run -p echo-lab --bin echo-lab --run-ignored only --locked --config-file .github/nextest.toml --profile ci --test-threads 2 --success-output immediate $(NEXTEST_SHARD)

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
	cargo build --locked -p echo-bridge --bins
	if [ "$$(uname -s)" = Linux ]; then python scripts/test_coverage_profiles.py --instrumented; fi
	cargo +nightly-2026-11-05 llvm-cov clean --workspace
	mkdir -p target/llvm-cov
	cargo +nightly-2026-11-05 llvm-cov --workspace --all-targets --locked --branch --no-report
	cargo +nightly-2026-11-05 llvm-cov -p echo-bridge --test bridge_t3 --locked --branch --no-report -- --ignored --nocapture
	cargo +nightly-2026-11-05 llvm-cov $(COVERAGE_HOST_T3) --locked --branch --no-report -- --ignored --nocapture
	cargo +nightly-2026-11-05 llvm-cov -p echo-lab --test sim_bridge_cli --locked --branch --no-report -- --ignored --nocapture
	cargo +nightly-2026-11-05 llvm-cov -p echo-lab --test sim_host --locked --branch --no-report -- --ignored --nocapture --test-threads=2
	cargo +nightly-2026-11-05 llvm-cov -p echo-lab --bin echo-lab --locked --branch --no-report -- --ignored --nocapture
	python tests/test_windows_profraw.py --writer target/llvm-cov-target/debug/echo-lab-fake-adb
	python tests/coverage_children.py
	python scripts/coverage_profiles.py
	cargo +nightly-2026-11-05 llvm-cov report --branch --no-default-ignore-filename-regex --ignore-filename-regex '$(COVERAGE_DEPENDENCY_SOURCES)' --fail-under-lines 80 --lcov --output-path target/llvm-cov/coverage.lcov
	cargo xtask coverage-check target/llvm-cov/coverage.lcov "$(COVERAGE_BASE)"

deps-fresh:
	cargo deny fetch && cargo deny check

# CI runs the two halves of build-host as parallel jobs; `make build-host` runs both.
.PHONY: build-host-workspace
build-host: build-host-workspace test-echo-install

build-host-workspace:
	cargo build --workspace --locked
	git diff --exit-code HEAD -- Cargo.lock
	$(MAKE) check-echo-screen

.PHONY: test-echo-install
test-echo-install:
	python -c "import resource,subprocess,sys,time; t=time.time(); r=subprocess.run([sys.executable,'tests/test_echo_install.py']); u=resource.getrusage(resource.RUSAGE_CHILDREN); print(f'TIMING wall={time.time()-t:.1f} user={u.ru_utime:.1f} sys={u.ru_stime:.1f} cpus={__import__(\"os\").cpu_count()}'); sys.exit(r.returncode)"

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

# echo-bridge and echo-link join when their manifests land on main; recheck at final rebase.
ECHO_PACKAGES := echo-contracts echo-config echo-wyoming echo-audio echo-live echo-chatgpt-auth echo-agent-api echo-agent-loop echo-mcp-auth echo-mcp-client echo-screen-contract echo-screen-server echo-delegation echo-signin $(foreach crate,echo-bridge echo-link,$(if $(wildcard crates/$(crate)/Cargo.toml),$(crate)))

.PHONY: build-echo
build-echo:
	sh scripts/build-echo.sh $(ECHO_PACKAGES)

ci: fmt-check lint-rust lint-shape lint-deps lint-rules lint-scripts-web lint-docs lint-ci-parity lint-self-tests test-unit test-contract test-sim-suites test-sim-host test-concurrency build-host build-echo

.PHONY: build-codex-armv7
build-codex-armv7:
	nice -n 10 sh scripts/build-codex-armv7.sh

DIST_VERSION ?= 0.0.0-dev.$(shell git rev-parse --short=12 HEAD)

.PHONY: dist test-release-screen
test-release-screen:
	python3 scripts/package-dist.py screen-proof

dist: test-release-screen
	python3 scripts/package-dist.py build --version "$(DIST_VERSION)"
