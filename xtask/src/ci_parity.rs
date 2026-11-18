use std::collections::BTreeSet;
use std::fs;
pub const GATES: &[(&str, &str)] = &[
    ("format", "fmt-check"),
    ("lint-rust", "lint-rust"),
    ("lint-shape", "lint-shape"),
    ("lint-deps", "lint-deps"),
    ("lint-rules", "lint-rules"),
    ("lint-scripts-web", "lint-scripts-web"),
    ("lint-docs", "lint-docs"),
    ("lint-ci-parity", "lint-ci-parity"),
    ("lint-self-tests", "lint-self-tests"),
    ("test-unit", "test-unit"),
    ("test-contract", "test-contract"),
    ("test-sim", "test-sim"),
    ("test-concurrency", "test-concurrency"),
    ("build-host", "build-host"),
    ("build-echo", "build-echo"),
];
const PREFIX: &str = "name: CI\non:\n  pull_request:\n  push:\n    branches: [main]\npermissions:\n  contents: read\ndefaults:\n  run:\n    shell: sh\njobs:\n";

// This intentionally accepts only the reviewed, explicit job grammar. YAML
// aliases, matrices, conditional gates and arbitrary shell are not equivalent.
fn job(name: &str, target: &str) -> String {
    let timeout = match name {
        "test-unit" => 15,
        "test-sim" => 25,
        "lint-self-tests" => 20,
        _ => 10,
    };
    let mut block = format!(
        "  {name}:\n    name: {name}\n    runs-on: ubuntu-latest\n    timeout-minutes: {timeout}\n    steps:\n      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - uses: ./.github/actions/setup\n        with:\n          target: {target}\n      - name: {name}\n        run: make {target}\n"
    );
    if matches!(name, "format" | "lint-scripts-web" | "test-unit") {
        let step = format!("      - name: {name}\n");
        let mut provision = String::from(
            "      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: |\n            ~/.npm\n            ~/.cache/ms-playwright\n          key: screen-${{ runner.os }}-${{ hashFiles('web/screen/package-lock.json') }}\n      - name: Provision locked screen npm cache\n        run: npm --prefix web/screen ci --offline=false\n",
        );
        if name == "test-unit" {
            provision.push_str("      - name: Provision pinned screen Chromium\n        run: cd web/screen && npx --offline playwright install --with-deps chromium\n");
        }
        block = block.replace(&step, &format!("{provision}{step}"));
    }
    if name == "test-sim" {
        let step = format!("      - name: {name}\n");
        let host = "      - name: Host build dependencies\n        run: sudo apt-get update && sudo apt-get install -y libopus-dev zlib1g-dev\n      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: .cache/hassmic-host\n          key: hassmic-host-${{ runner.os }}-${{ hashFiles('hassmic.pin') }}\n      - name: Build pinned hassmic-host\n        run: make hassmic-host\n";
        block = block.replace(&step, &format!("{host}{step}"));
    }
    if matches!(name, "test-unit" | "test-sim") {
        let step = format!("      - name: {name}\n");
        let codex = "      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n        with:\n          path: ~/.cache/portos-echo/codex\n          key: codex-${{ runner.os }}-${{ hashFiles('codex.pin') }}\n      - name: Provision verified pinned Codex\n        run: sh scripts/codex-app-server.sh\n";
        let namespace = "      - name: Enable isolated Codex network namespace\n        run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n";
        block = block.replace(&step, &format!("{codex}{namespace}{step}"));
    }
    if name == "test-unit" {
        block.push_str("      - name: Publish screen screenshot evidence\n        if: always()\n        id: screen-evidence\n        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02\n        with:\n          name: screen-evidence-${{ github.sha }}\n          path: |\n            web/screen/test-results/screenshots/**\n            web/screen/test-results/summary.txt\n          if-no-files-found: error\n");
    }
    if name == "build-echo" {
        let step = "      - name: build-echo\n";
        block = block.replace(step, &format!("      - name: Provision pinned Android Rust target\n        run: |\n          channel=$(python -c 'import tomllib; print(tomllib.load(open(\"rust-toolchain.toml\", \"rb\"))[\"toolchain\"][\"channel\"])')\n          rustup target add --toolchain \"$channel\" armv7-linux-androideabi\n{step}"));
        block.push_str("      - name: Publish Echo bridge\n        if: hashFiles('crates/echo-bridge/Cargo.toml') != ''\n        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02\n        with:\n          name: echo-bridge-armv7-android-${{ github.sha }}\n          path: target/armv7-linux-androideabi/release/echo-bridge\n          if-no-files-found: error\n");
    }
    block
}
fn make_targets(makefile: &str) -> Result<Vec<&str>, String> {
    let declarations: Vec<_> = makefile.lines().filter_map(|line| line.strip_prefix("ci:")).collect();
    if declarations.len() != 1 {
        return Err("Makefile: expected exactly one literal ci: prerequisite list".into());
    }
    let targets: Vec<_> = declarations[0].split_whitespace().collect();
    unique(&targets, "Makefile")?;
    Ok(targets)
}
fn unique(names: &[&str], source: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(format!("{source}: duplicate gate {name}"));
        }
    }
    Ok(())
}
fn ordered(actual: &[&str], expected: &[&str], source: &str) -> Result<(), String> {
    unique(actual, source)?;
    for name in expected {
        if !actual.contains(name) {
            return Err(format!("{source}: missing gate {name}"));
        }
    }
    for name in actual {
        if !expected.contains(name) {
            return Err(format!("{source}: extra or unsupported gate {name}"));
        }
    }
    if actual != expected {
        return Err(format!("{source}: gate order differs; expected {}", expected.join(" ")));
    }
    Ok(())
}
fn workflow_jobs(workflow: &str) -> Result<Vec<&str>, String> {
    let body = workflow
        .strip_prefix(PREFIX)
        .ok_or("ci.yml: unsupported workflow header")?;
    let mut blocks = Vec::new();
    let mut start = 0;
    for (offset, _) in body.match_indices("\n  ") {
        let tail = &body[offset + 3..];
        if !tail.starts_with(' ') {
            blocks.push(&body[start..=offset]);
            start = offset + 1;
        }
    }
    blocks.push(&body[start..]);
    Ok(blocks)
}

fn evaluate(makefile: &str, workflow: &str) -> Result<(), String> {
    let targets = make_targets(makefile)?;
    let expected: Vec<_> = GATES.iter().map(|(_, target)| *target).collect();
    ordered(&targets, &expected, "Makefile")?;
    let blocks = workflow_jobs(workflow)?;
    let names: Vec<_> = blocks
        .iter()
        .map(|block| block.lines().next().unwrap_or_default().trim().trim_end_matches(':'))
        .collect();
    let expected: Vec<_> = GATES.iter().map(|(name, _)| *name).collect();
    ordered(&names, &expected, "ci.yml")?;
    for (block, (name, target)) in blocks.iter().zip(GATES) {
        if *block != job(name, target) {
            return Err(format!(
                "ci.yml: unsupported job {name}; expected unconditional make {target} with pinned checkout/setup"
            ));
        }
    }
    Ok(())
}

// Repository workflows use block mappings. Track indentation and opaque block
// scalars so step env/with and shell text cannot become job-level env entries.
fn without_comment(value: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, ch) {
            (Some('"'), '\\') => escaped = true,
            (Some(current), ch) if current == ch => quote = None,
            (None, '\'' | '"') => quote = Some(ch),
            (None, '#') if offset == 0 || value[..offset].ends_with(char::is_whitespace) => return &value[..offset],
            _ => {}
        }
    }
    value
}

fn workflow_contexts(path: &str, workflow: &str) -> Result<(), String> {
    let mut scope: Vec<(usize, &str)> = Vec::new();
    let mut scalar = None;
    for (line, raw) in workflow.lines().enumerate() {
        let text = raw.trim_start();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let indent = raw.len() - text.len();
        if scalar.is_some_and(|level| indent > level) {
            continue;
        }
        scalar = None;
        while scope.last().is_some_and(|(level, _)| *level >= indent) {
            scope.pop();
        }
        let Some((key, value)) = text.split_once(':') else {
            continue;
        };
        let job_env = scope.len() >= 3 && scope[0].1 == "jobs" && scope[2].1 == "env";
        let inline_env = scope.len() == 2 && scope[0].1 == "jobs" && key == "env";
        if (job_env || inline_env) && runner_expression(value) {
            return Err(format!(
                "{path}:{}: runner context is unavailable in job-level env",
                line + 1
            ));
        }
        let value = without_comment(value).trim();
        if value.starts_with(['|', '>']) {
            scalar = Some(indent);
        }
        scope.push((indent, key));
    }
    Ok(())
}

fn runner_expression(value: &str) -> bool {
    let value = without_comment(value);
    value.split("${{").skip(1).any(|tail| {
        let Some((expression, _)) = tail.split_once("}}") else {
            return false;
        };
        // Single-quoted expression strings (including doubled quotes) are data.
        expression.split('\'').step_by(2).any(|code| {
            let spaced = code.replace('.', " . ").replace('[', " [ ");
            let tokens: Vec<_> = spaced
                .split(|c: char| !c.is_alphanumeric() && !matches!(c, '_' | '.' | '['))
                .filter(|token| !token.is_empty())
                .collect();
            tokens.windows(2).enumerate().any(|(i, pair)| {
                pair[0] == "runner" && matches!(pair[1], "." | "[") && (i == 0 || tokens[i - 1] != ".")
            })
        })
    })
}

fn check_workflows() -> Result<(), String> {
    let entries = fs::read_dir(".github/workflows").map_err(|e| format!(".github/workflows: {e}"))?;
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!(".github/workflows: {e}"))?;
    paths.sort();
    for path in paths {
        if !path.extension().is_some_and(|ext| ext == "yml" || ext == "yaml") {
            continue;
        }
        let name = path.to_string_lossy().replace('\\', "/");
        let workflow = fs::read_to_string(&path).map_err(|e| format!("{name}: {e}"))?;
        workflow_contexts(&name, &workflow)?;
    }
    Ok(())
}

pub fn run() -> Result<(), String> {
    let makefile = fs::read_to_string("Makefile").map_err(|error| format!("Makefile: {error}"))?;
    let workflow = fs::read_to_string(".github/workflows/ci.yml").map_err(|error| format!("ci.yml: {error}"))?;
    evaluate(&makefile.replace("\r\n", "\n"), &workflow.replace("\r\n", "\n"))?;
    check_workflows()?;
    println!("ci-parity: ok ({} checks)", GATES.len());
    for (name, target) in GATES {
        println!("{name}: make {target}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (String, String) {
        (
            format!("ci: {}\n", GATES.iter().map(|(_, t)| *t).collect::<Vec<_>>().join(" ")),
            format!("{PREFIX}{}", GATES.iter().map(|(n, t)| job(n, t)).collect::<String>()),
        )
    }
    fn assert_failure(makefile: &str, workflow: &str, reason: &str) {
        let error = evaluate(makefile, workflow).unwrap_err();
        assert!(error.contains(reason), "{error}");
    }
    #[test]
    fn workflow_contexts_rejects_job_env_and_preserves_step_scope() {
        let fixed = include_str!("../../.github/workflows/build-codex-armv7.yml");
        for newline in ["\n", "\r\n"] {
            assert_eq!(workflow_contexts("arm.yml", &fixed.replace('\n', newline)), Ok(()));
            for expression in [
                "${{ runner.temp }}",
                "${{runner.temp}}",
                "${{ runner . temp }}",
                "${{ runner['temp'] }}",
            ] {
                for key in ["CODEX_ARMV7_WORK_DIR", "CODEX_ARMV7_OUTPUT_DIR"] {
                    let yaml = format!("jobs:\n  build:\n    env:\n      {key}: {expression}\n").replace('\n', newline);
                    assert_eq!(
                        workflow_contexts("arm.yml", &yaml),
                        Err("arm.yml:4: runner context is unavailable in job-level env".into())
                    );
                }
            }
        }
        let allowed = "jobs:\n  build:\n    env:\n      TEXT: runner.temp\n      QUOTED: ${{ 'runner.temp' }}\n      OTHER: ${{ github . runner . temp }}\n      # COMMENT: ${{ runner.temp }}\n      SAFE: literal # ${{ runner.temp }}\n    steps:\n      - run: |\n          env:\n            TEXT: ${{ runner.temp }}\n        env:\n          DIR: ${{ runner.temp }}\n        with:\n          path: ${{ runner.temp }}/output\n  next:\n    steps:\n      - run: true\n        env:\n          DIR: ${{ runner.temp }}\n";
        assert_eq!(workflow_contexts("arm.yml", allowed), Ok(()));
        let invalid = format!("{allowed}    env:\n      DIR: ${{{{ runner.temp }}}}\n");
        assert!(
            workflow_contexts("arm.yml", &invalid)
                .unwrap_err()
                .contains("runner context is unavailable")
        );
    }
    #[test]
    fn matching_ordered_gates_pass() {
        let (makefile, workflow) = fixture();
        assert_eq!(evaluate(&makefile, &workflow), Ok(()));
    }
    #[test]
    fn codex_provisioning_cannot_be_removed_or_made_optional() {
        let (makefile, workflow) = fixture();
        for name in ["test-unit", "test-sim"] {
            let block = job(name, name);
            for replacement in ["", "        run: sh scripts/codex-app-server.sh || true\n"] {
                let changed = block.replace("        run: sh scripts/codex-app-server.sh\n", replacement);
                assert!(evaluate(&makefile, &workflow.replace(&block, &changed)).is_err());
            }
            let namespace = "        run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n";
            for replacement in [String::new(), format!("{namespace}        if: false\n")] {
                let changed = block.replace(namespace, &replacement);
                assert!(evaluate(&makefile, &workflow.replace(&block, &changed)).is_err());
            }
        }
    }
    #[test]
    fn echo_gate_requires_exact_provisioning_build_and_publication() {
        let (makefile, workflow) = fixture();
        let echo = job("build-echo", "build-echo");
        for changed in [
            String::new(),
            format!("{echo}{echo}"),
            echo.replace("    name: build-echo", "    if: false\n    name: build-echo"),
            echo.replace("run: make build-echo", "run: make build-echo || true"),
            echo.replace("armv7-linux-androideabi", "aarch64-linux-android"),
            echo.replace("rustup target add --toolchain", "rustup target add"),
            echo.replace("          if-no-files-found: error\n", ""),
            echo.replace("crates/echo-bridge/Cargo.toml", "crates/other/Cargo.toml"),
            echo.replace("target/armv7-linux-androideabi/release/echo-bridge", "target/stale"),
        ] {
            assert!(evaluate(&makefile, &workflow.replace(&echo, &changed)).is_err());
        }
        assert!(evaluate(&makefile.replace(" build-echo", ""), &workflow).is_err());
        let reordered = workflow.replace(
            &(job("build-host", "build-host") + &echo),
            &(echo + &job("build-host", "build-host")),
        );
        assert!(evaluate(&makefile, &reordered).is_err());
    }
    #[test]
    fn missing_extra_duplicate_and_reordered_gates_fail() {
        let (makefile, workflow) = fixture();
        for (input, diagnostic) in [
            (makefile.replace("lint-shape ", ""), "missing gate lint-shape"),
            (format!("{makefile}ci: other\n"), "exactly one"),
            (
                makefile.replace("build-host", "build-host surprise"),
                "extra or unsupported gate surprise",
            ),
            (
                makefile.replace("build-host", "build-host build-host"),
                "duplicate gate build-host",
            ),
            (
                makefile.replace("test-unit test-contract", "test-contract test-unit"),
                "gate order differs",
            ),
        ] {
            assert_failure(&input, &workflow, diagnostic);
        }
        let missing = workflow.replace(&job("lint-shape", "lint-shape"), "");
        assert_failure(&makefile, &missing, "missing gate lint-shape");
        let duplicate = format!("{workflow}{}", job("format", "fmt-check"));
        assert_failure(&makefile, &duplicate, "duplicate gate format");
        let extra = format!("{workflow}{}", job("extra", "extra"));
        assert_failure(&makefile, &extra, "unsupported gate extra");
        // The owner suspended coverage on 2026-11-18; it is no longer a CI gate.
        assert_failure(
            &makefile.replace("build-host", "coverage build-host"),
            &workflow,
            "extra or unsupported gate coverage",
        );
        let reordered = workflow.replace(
            &(job("test-unit", "test-unit") + &job("test-contract", "test-contract")),
            &(job("test-contract", "test-contract") + &job("test-unit", "test-unit")),
        );
        assert_failure(&makefile, &reordered, "gate order differs");
    }
    #[test]
    fn unsupported_yaml_and_shell_fail_closed() {
        let (makefile, workflow) = fixture();
        for input in [
            workflow.replace("run: make fmt-check", "run: make fmt-check || true"),
            workflow.replace("    name: format", "    if: false\n    name: format"),
            workflow.replace("name: CI", "name: Other"),
            String::new(),
        ] {
            assert!(evaluate(&makefile, &input).is_err());
        }
        assert_failure("ci: $(GATES)", &workflow, "missing gate");
    }
    #[test]
    fn screen_provisioning_requires_exact_steps_and_unconditional_gates() {
        let (makefile, workflow) = fixture();
        for fragment in [
            "            ~/.npm\n",
            "            ~/.cache/ms-playwright\n",
            "          key: screen-${{ runner.os }}-${{ hashFiles('web/screen/package-lock.json') }}\n",
            "      - name: Provision locked screen npm cache\n",
            "        run: npm --prefix web/screen ci --offline=false\n",
            "      - name: Provision pinned screen Chromium\n",
            "        run: cd web/screen && npx --offline playwright install --with-deps chromium\n",
            "        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02\n",
            "          name: screen-evidence-${{ github.sha }}\n",
            "            web/screen/test-results/screenshots/**\n",
            "            web/screen/test-results/summary.txt\n",
            "          if-no-files-found: error\n",
        ] {
            for replacement in [String::new(), format!("{fragment}        if: false\n")] {
                assert!(evaluate(&makefile, &workflow.replace(fragment, &replacement)).is_err());
            }
        }
        for (from, to) in [
            (
                "hashFiles('web/screen/package-lock.json')",
                "hashFiles('web/screen/package.json')",
            ),
            (
                "npm --prefix web/screen ci --offline=false",
                "npm --prefix web/screen install",
            ),
            (
                "playwright install --with-deps chromium",
                "playwright install --with-deps firefox",
            ),
            ("run: make test-unit\n", "run: make test-unit || true\n"),
        ] {
            assert!(evaluate(&makefile, &workflow.replace(from, to)).is_err());
        }
    }
    #[test]
    fn host_jobs_require_each_exact_host_step() {
        let (makefile, workflow) = fixture();
        for name in ["test-sim"] {
            let host_job = job(name, name);
            let gate = format!("        run: make {name}\n");
            let fragments = [
                "      - name: Host build dependencies\n",
                "        run: sudo apt-get update && sudo apt-get install -y libopus-dev zlib1g-dev\n",
                "      - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n",
                "          path: .cache/hassmic-host\n",
                "          key: hassmic-host-${{ runner.os }}-${{ hashFiles('hassmic.pin') }}\n",
                "      - name: Build pinned hassmic-host\n",
                "        run: make hassmic-host\n",
                &gate,
            ];
            for fragment in fragments {
                for replacement in [String::new(), format!("{fragment}        if: false\n")] {
                    assert!(
                        evaluate(
                            &makefile,
                            &workflow.replace(&host_job, &host_job.replace(fragment, &replacement))
                        )
                        .unwrap_err()
                        .contains(&format!("unsupported job {name}"))
                    );
                }
            }
            for (from, to) in [
                ("libopus-dev", "libopus0"),
                ("zlib1g-dev", "zlib1g"),
                ("0057852bfaa89a56745cba8c7296529d2fc39830", "v4"),
                ("path: .cache/hassmic-host", "path: .cache"),
                ("hashFiles('hassmic.pin')", "hashFiles('Cargo.lock')"),
                ("run: make hassmic-host\n", "run: make hassmic-host || true\n"),
            ] {
                assert!(evaluate(&makefile, &workflow.replace(&host_job, &host_job.replace(from, to))).is_err());
            }
            let conditional = format!("        run: make {name} || true\n");
            assert!(
                evaluate(
                    &makefile,
                    &workflow.replace(&host_job, &host_job.replace(&gate, &conditional))
                )
                .is_err()
            );
        }
    }
}
