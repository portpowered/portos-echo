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
    ("coverage", "coverage"),
    ("build-host", "build-host"),
];

const PREFIX: &str = "name: CI\non:\n  pull_request:\n  push:\n    branches: [main]\npermissions:\n  contents: read\ndefaults:\n  run:\n    shell: sh\njobs:\n";

// This intentionally accepts only the reviewed, explicit job grammar. YAML
// aliases, matrices, conditional gates and arbitrary shell are not equivalent.
fn job(name: &str, target: &str) -> String {
    let mut block = format!(
        "  {name}:\n    name: {name}\n    runs-on: ubuntu-latest\n    timeout-minutes: 10\n    steps:\n      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - uses: ./.github/actions/setup\n        with:\n          target: {target}\n      - name: {name}\n        run: make {target}\n"
    );
    if name == "coverage" {
        block.push_str(
            "        env:\n          COVERAGE_BASE: ${{ github.event.pull_request.base.sha || github.event.before }}\n",
        );
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

pub fn run() -> Result<(), String> {
    let makefile = fs::read_to_string("Makefile").map_err(|error| format!("Makefile: {error}"))?;
    let workflow = fs::read_to_string(".github/workflows/ci.yml").map_err(|error| format!("ci.yml: {error}"))?;
    evaluate(&makefile.replace("\r\n", "\n"), &workflow.replace("\r\n", "\n"))?;
    println!("ci-parity: ok (15 checks)");
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

    #[test]
    fn matching_ordered_gates_pass() {
        let (makefile, workflow) = fixture();
        assert_eq!(evaluate(&makefile, &workflow), Ok(()));
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
            assert!(evaluate(&input, &workflow).unwrap_err().contains(diagnostic));
        }
        let missing = workflow.replace(&job("lint-shape", "lint-shape"), "");
        assert!(
            evaluate(&makefile, &missing)
                .unwrap_err()
                .contains("missing gate lint-shape")
        );
        let duplicate = format!("{workflow}{}", job("format", "fmt-check"));
        assert!(
            evaluate(&makefile, &duplicate)
                .unwrap_err()
                .contains("duplicate gate format")
        );
        let extra = format!("{workflow}{}", job("extra", "extra"));
        assert!(
            evaluate(&makefile, &extra)
                .unwrap_err()
                .contains("unsupported gate extra")
        );
        let reordered = workflow.replace(
            &(job("test-unit", "test-unit") + &job("test-contract", "test-contract")),
            &(job("test-contract", "test-contract") + &job("test-unit", "test-unit")),
        );
        assert!(
            evaluate(&makefile, &reordered)
                .unwrap_err()
                .contains("gate order differs")
        );
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
        assert!(
            evaluate("ci: $(GATES)", &workflow)
                .unwrap_err()
                .contains("missing gate")
        );
    }
}
