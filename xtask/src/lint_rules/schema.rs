use std::{fs, process::Command};

use super::Names;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Witness {
    package: &'static str,
    target: &'static str,
    family: &'static str,
}

const WITNESSES: [Witness; 2] = [
    Witness {
        package: "echo-contracts",
        target: "contracts_schema",
        family: "contracts",
    },
    Witness {
        package: "echo-screen-contract",
        target: "screen_schema",
        family: "screen",
    },
];

pub(super) fn run(packages: &Names) -> Result<(), String> {
    let tracked = Command::new("git")
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| format!("schemas/:1: git ls-files: {error}"))?;
    if !tracked.status.success() {
        return Err(format!(
            "schemas/:1: git ls-files: {}",
            String::from_utf8_lossy(&tracked.stderr)
        ));
    }
    let tracked = String::from_utf8(tracked.stdout).map_err(|error| format!("schemas/:1: git ls-files: {error}"))?;
    check(
        packages,
        &tracked,
        |path| fs::read_to_string(path).map(|_| ()),
        |command| {
            command.output().map(|output| {
                (
                    output.status.success(),
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ),
                )
            })
        },
    )
}

fn check(
    packages: &Names,
    tracked: &str,
    mut read: impl FnMut(&str) -> std::io::Result<()>,
    mut execute: impl FnMut(&mut Command) -> std::io::Result<(bool, String)>,
) -> Result<(), String> {
    for witness in WITNESSES {
        if !applicable(witness, packages, tracked) {
            continue;
        }
        let manifest = format!("crates/{}/Cargo.toml", witness.package);
        let source = format!("crates/{}/tests/{}.rs", witness.package, witness.target);
        for path in [&manifest, &source] {
            read(path).map_err(|error| format!("{path}:1: schema witness input: {error}"))?;
        }
        let (success, output) = execute(&mut command(witness))
            .map_err(|error| format!("{source}:1: schema witness could not start: {error}"))?;
        outcome(witness, success, &output)?;
    }
    Ok(())
}

fn applicable(witness: Witness, packages: &Names, tracked: &str) -> bool {
    let crate_prefix = format!("crates/{}/", witness.package);
    let schema_prefix = format!("schemas/{}/", witness.family);
    packages.contains(witness.package)
        || tracked
            .split('\0')
            .any(|path| path.starts_with(&crate_prefix) || path.starts_with(&schema_prefix))
}

fn command(witness: Witness) -> Command {
    let mut command = Command::new("cargo");
    command
        .args([
            "test",
            "--locked",
            "--offline",
            "-p",
            witness.package,
            "--test",
            witness.target,
            "schema_snapshots",
            "--",
            "--exact",
        ])
        .env_remove("UPDATE_SCHEMAS")
        .env("CARGO_TERM_COLOR", "never");
    command
}

fn outcome(witness: Witness, success: bool, output: &str) -> Result<(), String> {
    let source = format!("crates/{}/tests/{}.rs", witness.package, witness.target);
    if success
        && output.lines().any(|line| line.trim() == "test schema_snapshots ... ok")
        && output.contains("test result: ok. 1 passed; 0 failed;")
    {
        return Ok(());
    }
    let normalized = output.replace("\\\\", "/").replace('\\', "/");
    let schema = normalized.find("schemas/").and_then(|start| {
        let tail = &normalized[start..];
        tail.find(".json").map(|end| &tail[..end + 5])
    });
    let diagnostic = schema.map_or_else(
        || format!("{source}:1: schema witness failed or did not execute schema_snapshots"),
        |path| format!("{path}:1: schema-drift: committed schema missing or differs from generated types"),
    );
    Err(format!("{diagnostic}\n{output}"))
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
