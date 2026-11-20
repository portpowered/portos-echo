use std::collections::BTreeSet;
use std::fs;
mod shards;
mod shared;
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
    ("test-sim-suites", "test-sim-suites"),
    ("test-sim-host", "test-sim-host"),
    ("test-concurrency", "test-concurrency"),
    ("build-host", "build-host"),
    ("build-echo", "build-echo"),
];
const PREFIX: &str = "name: CI\non:\n  pull_request:\n  push:\n    branches: [main]\npermissions:\n  contents: read\ndefaults:\n  run:\n    shell: sh\njobs:\n";
// Only the reviewed job grammar is accepted; aliases, matrices and conditional gates are not
// equivalent. The split and deferred gates in `shards` are the reviewed exceptions.
fn job(name: &str, target: &str) -> String {
    if let Some(parts) = shards::parts(name) {
        return shards::gate(name, parts);
    }
    let timeout = if name == "lint-self-tests" { 20 } else { 10 };
    let mut block = format!(
        "  {name}:\n    name: {name}\n{}    runs-on: ubuntu-latest\n    timeout-minutes: {timeout}\n    steps:\n{}      - name: {name}\n        run: make {target}\n",
        shards::deferral(name),
        shards::setup(target, false)
    );
    let step = format!("      - name: {name}\n");
    if matches!(name, "format" | "lint-scripts-web") {
        block = block.replace(&step, &format!("{}{step}", shards::SCREEN));
    }
    if name == "build-echo" {
        block = block.replace(&step, &format!("      - name: Provision pinned Android Rust target\n        run: |\n          channel=$(python -c 'import tomllib; print(tomllib.load(open(\"rust-toolchain.toml\", \"rb\"))[\"toolchain\"][\"channel\"])')\n          rustup target add --toolchain \"$channel\" armv7-linux-androideabi\n{step}"));
        block.push_str("      - name: Publish Echo bridge\n        if: hashFiles('crates/echo-bridge/Cargo.toml') != ''\n        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02\n        with:\n          name: echo-bridge-armv7-android-${{ github.sha }}\n          path: target/armv7-linux-androideabi/release/echo-bridge\n          if-no-files-found: error\n");
    }
    block
}
// Part jobs come first so their runners are requested before the short gates'.
fn canonical() -> Vec<(String, String)> {
    let parts = GATES
        .iter()
        .filter_map(|(name, _)| shards::parts(name))
        .flatten()
        .map(|id| ((*id).to_string(), shards::part(id)));
    let gates = GATES
        .iter()
        .map(|(name, target)| ((*name).to_string(), job(name, target)));
    parts.chain(gates).collect()
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
    if let Some(rule) = shards::MAKE_SPLITS
        .iter()
        .find(|rule| makefile.matches(*rule).count() != 1)
    {
        return Err(format!(
            "Makefile: split gate differs; expected exactly once: {}",
            rule.trim_end()
        ));
    }
    let blocks = workflow_jobs(workflow)?;
    let names: Vec<_> = blocks
        .iter()
        .map(|block| block.lines().next().unwrap_or_default().trim().trim_end_matches(':'))
        .collect();
    let canonical = canonical();
    let expected: Vec<_> = canonical.iter().map(|(name, _)| name.as_str()).collect();
    ordered(&names, &expected, "ci.yml")?;
    for (block, (name, text)) in blocks.iter().zip(&canonical) {
        if *block != text {
            return Err(format!(
                "ci.yml: unsupported job {name}; expected unconditional make with pinned checkout/setup"
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
    shared::check()?;
    evaluate(&makefile.replace("\r\n", "\n"), &workflow.replace("\r\n", "\n"))?;
    check_workflows()?;
    println!("ci-parity: ok ({} checks)", GATES.len());
    for (name, target) in GATES {
        println!("{name}: make {target}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
