// Gates that CI runs as parallel part jobs. Every part runs one slice of the gate's Make target,
// and together the parts run all of it. The job named after the gate is the required check: it
// runs after its parts and passes only when every part passed, so a failed or cancelled part
// fails the gate instead of skipping it.

pub(super) const CHECKOUT: &str = "      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n";
pub(super) const SCREEN: &str = "      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: |\n            ~/.npm\n            ~/.cache/ms-playwright\n          key: screen-${{ runner.os }}-${{ hashFiles('web/screen/package-lock.json') }}\n      - name: Provision locked screen npm cache\n        run: npm --prefix web/screen ci --offline=false\n";
pub(super) const CHROMIUM: &str = "      - name: Provision pinned screen Chromium\n        run: cd web/screen && npx --offline playwright install --with-deps chromium\n";
// The host packages' .deb files are cached: some apt mirror downloads crawl at about 50 kB/s.
pub(super) const HOST: &str = "      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: .cache/apt-archives\n          key: sim-apt-${{ runner.os }}-v1\n      - name: Host build dependencies\n        run: |\n          mkdir -p .cache/apt-archives/partial\n          sudo apt-get update\n          sudo apt-get install -y -o Dir::Cache::archives=\"$PWD/.cache/apt-archives\" -o APT::Keep-Downloaded-Packages=true libopus-dev zlib1g-dev\n          sudo chown -R \"$(id -u):$(id -g)\" .cache/apt-archives\n      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: .cache/hassmic-host\n          key: hassmic-host-${{ runner.os }}-${{ hashFiles('hassmic.pin') }}\n      - name: Build pinned hassmic-host\n        run: make hassmic-host\n";
pub(super) const CODEX: &str = "      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: ~/.cache/portos-echo/codex\n          key: codex-${{ runner.os }}-${{ hashFiles('codex.pin') }}\n      - name: Provision verified pinned Codex\n        run: sh scripts/codex-app-server.sh\n";
pub(super) const NAMESPACE: &str = "      - name: Enable isolated Codex network namespace\n        run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n";
const SANDBOX: &str = "      - name: Chromium sandbox user namespaces\n        run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n";
const EVIDENCE: &str = "      - name: Publish screen screenshot evidence\n        if: always()\n        id: screen-evidence\n        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02\n        with:\n          name: screen-evidence-${{ github.sha }}\n          path: |\n            web/screen/test-results/screenshots/**\n            web/screen/test-results/summary.txt\n          if-no-files-found: error\n";
const PYTHON: &str = "      - uses: actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065\n        with:\n          python-version: '3.12'\n";

// The Make target of each split gate must be exactly its parts, so `make <gate>` runs what the
// parts run in CI. NEXTEST_SHARD is empty unless a part names its partition.
pub(super) const MAKE_SPLITS: [&str; 6] = [
    "NEXTEST_SHARD = $(if $(NEXTEST_PARTITION),--partition $(NEXTEST_PARTITION) --no-tests=pass)\n",
    "test-unit: test-unit-rust test-unit-python test-unit-screen\n",
    "test-unit-rust:\n\tcargo nextest run --workspace --all-targets --locked --config-file .github/nextest.toml --profile ci $(NEXTEST_SHARD)\n\n",
    "test-sim-suites: test-sim-scenarios test-sim-bridge test-sim-components\n",
    "test-sim-host: sim-prereqs\n\tcargo nextest run -p echo-lab --test sim_host --run-ignored only --locked --config-file .github/nextest.toml --profile ci --test-threads 2 --success-output immediate $(NEXTEST_SHARD)\n\tcargo nextest run -p echo-lab --bin echo-lab --run-ignored only --locked --config-file .github/nextest.toml --profile ci --test-threads 2 --success-output immediate $(NEXTEST_SHARD)\n\n",
    "build-host: build-host-workspace test-echo-install\n",
];

// Split gates and their part job ids, in workflow order.
pub(super) const SPLIT: [(&str, &[&str]); 4] = [
    ("test-unit", &["test-unit-rust", "test-unit-screen"]),
    (
        "test-sim-suites",
        &["test-sim-scenarios-shards", "test-sim-suites-parts"],
    ),
    ("test-sim-host", &["test-sim-host-shards"]),
    ("build-host", &["build-host-workspace", "test-echo-install"]),
];

pub(super) fn parts(gate: &str) -> Option<&'static [&'static str]> {
    SPLIT.iter().find(|(name, _)| *name == gate).map(|(_, parts)| *parts)
}

fn header(id: &str, name: &str, timeout: u32, matrix: Option<(&str, &str)>) -> String {
    let strategy = matrix.map_or_else(String::new, |(key, values)| {
        format!("    strategy:\n      fail-fast: false\n      matrix:\n        {key}: [{values}]\n")
    });
    format!(
        "  {id}:\n    name: {name}\n    runs-on: ubuntu-latest\n    timeout-minutes: {timeout}\n{strategy}    steps:\n"
    )
}

pub(super) fn setup(target: &str, sim: bool) -> String {
    let sim = if sim { "          sim: 'true'\n" } else { "" };
    format!("{CHECKOUT}      - uses: ./.github/actions/setup\n        with:\n          target: {target}\n{sim}")
}

fn step(name: &str, run: &str) -> String {
    format!("      - name: {name}\n        run: {run}\n")
}

// One canonical part job per id; every partition from 1 to N appears exactly once.
pub(super) fn part(id: &str) -> String {
    match id {
        "test-unit-rust" => {
            let mut block = header(
                id,
                "test-unit-rust ${{ matrix.shard }}/4",
                15,
                Some(("shard", "1, 2, 3, 4")),
            );
            block.push_str(&setup(id, false));
            block.push_str(CODEX);
            block.push_str(NAMESPACE);
            block + &step(id, "make test-unit-rust NEXTEST_PARTITION=count:${{ matrix.shard }}/4")
        }
        "test-unit-screen" => {
            let mut block = header(id, id, 15, None);
            block.push_str(&setup(id, false));
            block.push_str(SCREEN);
            block.push_str(CHROMIUM);
            block.push_str(SANDBOX);
            block.push_str(&step(id, "make test-unit-python test-unit-screen"));
            block + EVIDENCE
        }
        "test-sim-scenarios-shards" => {
            let block = header(
                id,
                "test-sim-scenarios ${{ matrix.shard }}/2",
                15,
                Some(("shard", "1, 2")),
            );
            let run = "make test-sim-scenarios SCENARIO_PARTITION=${{ matrix.shard }}/2";
            block + &setup("test-sim-suites", true) + &step("test-sim-suites", run)
        }
        "test-sim-suites-parts" => {
            let parts = "test-sim-bridge, test-sim-components";
            let block = header(id, "${{ matrix.part }}", 15, Some(("part", parts)));
            block + &setup("test-sim-suites", true) + &step("test-sim-suites", "make ${{ matrix.part }}")
        }
        "test-sim-host-shards" => {
            let block = header(
                id,
                "test-sim-host ${{ matrix.shard }}/3",
                15,
                Some(("shard", "1, 2, 3")),
            );
            let run = "make test-sim-host NEXTEST_PARTITION=count:${{ matrix.shard }}/3";
            block + &setup("test-sim-host", true) + &step("test-sim-host", run)
        }
        "build-host-workspace" => {
            header(id, id, 10, None) + &setup("build-host", false) + &step(id, "make build-host-workspace")
        }
        "test-echo-install" => header(id, id, 10, None) + CHECKOUT + PYTHON + &step(id, "make test-echo-install"),
        _ => String::new(),
    }
}

// The required check: it fails unless every part job succeeded.
pub(super) fn gate(name: &str, parts: &[&str]) -> String {
    let needs = parts.join(", ");
    let checks: Vec<_> = parts
        .iter()
        .map(|part| format!("test '${{{{ needs.{part}.result }}}}' = success"))
        .collect();
    format!(
        "  {name}:\n    name: {name}\n    needs: [{needs}]\n    if: always()\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n{}",
        step(name, &checks.join(" && "))
    )
}
