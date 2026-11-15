mod lcov;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::Command;

type Changed = BTreeMap<String, BTreeSet<u32>>;

fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("git output: {error}"))
}

fn changed(diff: &str) -> Result<Changed, String> {
    let mut result = Changed::new();
    let mut path = None;
    for line in diff.lines() {
        if let Some(value) = line.strip_prefix("+++ ") {
            path = value.strip_prefix("b/").map(str::to_owned);
            if path.is_none() && value != "/dev/null" {
                return Err(format!("unsupported diff path {value}"));
            }
        } else if line.starts_with("@@ ") {
            let range = line
                .split_whitespace()
                .nth(2)
                .and_then(|value| value.strip_prefix('+'))
                .ok_or("invalid diff hunk")?;
            let mut parts = range.split(',');
            let start: u32 = parts
                .next()
                .ok_or("missing diff start")?
                .parse()
                .map_err(|_| "invalid diff start")?;
            let count: u32 = parts.next().unwrap_or("1").parse().map_err(|_| "invalid diff count")?;
            let end = start.checked_add(count).ok_or("diff range overflow")?;
            if let Some(path) = &path {
                result.entry(path.clone()).or_default().extend(start..end);
            }
        }
    }
    Ok(result)
}

fn floor(label: &str, hits: usize, total: usize, percent: usize) -> Result<String, String> {
    let actual = (hits * 10000).checked_div(total).unwrap_or(10000);
    let message = format!(
        "{label}: {hits}/{total} ({}.{:02}%), floor {percent}%",
        actual / 100,
        actual % 100
    );
    if hits * 100 < total * percent {
        Err(message)
    } else {
        Ok(message)
    }
}

fn counts<'a>(files: impl Iterator<Item = &'a lcov::File>) -> (usize, usize, usize, usize) {
    files.fold((0, 0, 0, 0), |(lh, lf, bh, bf), file| {
        (
            lh + file.lines.values().filter(|hit| **hit).count(),
            lf + file.lines.len(),
            bh + file.branches.values().filter(|hit| **hit).count(),
            bf + file.branches.len(),
        )
    })
}

fn scope(label: &str, counts: (usize, usize, usize, usize), floors: (usize, usize)) -> Result<Vec<String>, String> {
    let (lh, lf, bh, bf) = counts;
    if lf == 0 {
        return Err(format!("{label}: missing executable line evidence"));
    }
    Ok(vec![
        floor(&format!("{label} lines"), lh, lf, floors.0)?,
        floor(&format!("{label} branches"), bh, bf, floors.1)?,
    ])
}

fn evaluate(report: &lcov::Report, changes: &Changed, crates: &[(String, String)]) -> Result<Vec<String>, String> {
    let mut messages = scope("workspace", counts(report.values()), (80, 70))?;
    for (name, prefix) in crates {
        let floors = match name.as_str() {
            "echo-contracts" | "echo-config" | "echo-screen-contract" => (95, 90),
            "echo-mcp-auth" | "echo-mcp-client" | "echo-agent-loop" | "echo-wyoming" | "echo-bridge" => (90, 85),
            _ => (0, 0),
        };
        messages.extend(scope(
            name,
            counts(
                report
                    .iter()
                    .filter(|(path, _)| path.starts_with(prefix))
                    .map(|(_, file)| file),
            ),
            floors,
        )?);
    }
    let mut hits = 0;
    let mut total = 0;
    for (path, lines) in changes {
        if let Some(file) = report.get(path) {
            for (line, hit) in &file.lines {
                if lines.contains(line) {
                    total += 1;
                    hits += usize::from(*hit);
                }
            }
        } else if !lines.is_empty() {
            return Err(format!("changed Rust source missing from coverage: {path}"));
        }
    }
    messages.push(floor("changed executable Rust lines", hits, total, 85)?);
    Ok(messages)
}

fn crates(root: &str) -> Result<Vec<(String, String)>, String> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .map_err(|error| format!("cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|error| format!("metadata: {error}"))?;
    let packages = metadata["packages"].as_array().ok_or("missing metadata packages")?;
    packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().ok_or("missing package name")?;
            let manifest = package["manifest_path"]
                .as_str()
                .ok_or("missing manifest path")?
                .replace('\\', "/");
            let prefix = manifest
                .strip_prefix(&format!("{root}/"))
                .and_then(|path| path.strip_suffix("Cargo.toml"))
                .ok_or("manifest outside root")?;
            Ok((name.to_owned(), prefix.to_owned()))
        })
        .collect()
}

pub fn run(path: &str, base: &str) -> Result<(), String> {
    let root = git(&["rev-parse", "--show-toplevel"])?.trim().replace('\\', "/");
    let base = git(&["rev-parse", "--verify", &format!("{base}^{{commit}}")])?;
    let base = base.trim();
    let report = lcov::parse(
        &fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?,
        &root,
    )?;
    let diff = git(&[
        "diff",
        "--no-ext-diff",
        "--no-renames",
        "--unified=0",
        "--no-color",
        base,
        "--",
        "*.rs",
        ":(exclude)factory/",
    ])?;
    let changes = changed(&diff)?;
    println!("coverage base: {base}");
    for message in evaluate(&report, &changes, &crates(&root)?)? {
        println!("{message}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> lcov::Report {
        lcov::parse("SF:xtask/src/main.rs\nDA:1,1\nDA:2,1\nDA:3,1\nDA:4,1\nDA:5,0\nBRDA:1,0,0,1\nBRDA:1,0,1,1\nBRDA:2,0,0,1\nBRDA:2,0,1,0\nend_of_record", "/repo").unwrap()
    }

    #[test]
    fn exact_floor_boundaries_and_absent_branches() {
        assert!(floor("lines", 80, 100, 80).is_ok());
        assert!(floor("lines", 79, 100, 80).unwrap_err().contains("79/100"));
        assert!(floor("branches", 70, 100, 70).is_ok());
        assert!(floor("branches", 69, 100, 70).is_err());
        assert!(floor("changed", 85, 100, 85).is_ok());
        assert!(floor("changed", 84, 100, 85).is_err());
        assert!(floor("branches", 0, 0, 70).is_ok());
        assert!(scope("empty", (0, 0, 0, 0), (80, 70)).is_err());
    }

    #[test]
    fn changed_ranges_cover_additions_not_deletions_or_context() {
        let diff = "+++ b/xtask/src/main.rs\n@@ -1 +1,2 @@\n+x\n+y\n@@ -5,0 +7 @@\n+z\n+++ /dev/null\n@@ -1 +0,0 @@\n";
        assert_eq!(changed(diff).unwrap()["xtask/src/main.rs"], BTreeSet::from([1, 2, 7]));
        assert!(changed("+++ \"b/path\"\n").is_err());
        for hunk in [
            "@@ -1 +bad @@",
            "@@ -1 +1,bad @@",
            "@@ -1 +4294967295,2 @@",
            "@@ broken",
        ] {
            assert!(changed(hunk).is_err());
        }
    }

    #[test]
    fn policy_rejects_branch_drop_changed_line_drop_and_missing_sources() {
        let mut report = report();
        let crates = vec![("xtask".into(), "xtask/".into())];
        let changes = Changed::from([("xtask/src/main.rs".into(), BTreeSet::from([1, 2]))]);
        assert_eq!(evaluate(&report, &changes, &crates).unwrap().len(), 5);
        let uncovered = Changed::from([("xtask/src/main.rs".into(), BTreeSet::from([5]))]);
        assert!(
            evaluate(&report, &uncovered, &crates)
                .unwrap_err()
                .contains("changed executable Rust lines")
        );
        let missing = Changed::from([("new.rs".into(), BTreeSet::from([1]))]);
        assert!(
            evaluate(&report, &missing, &crates)
                .unwrap_err()
                .contains("missing from coverage")
        );
        report
            .get_mut("xtask/src/main.rs")
            .unwrap()
            .branches
            .insert((2, 0, 0), false);
        assert!(
            evaluate(&report, &changes, &crates)
                .unwrap_err()
                .contains("workspace branches: 2/4")
        );
    }

    #[test]
    fn named_crate_floors_are_enforced_as_crates_appear() {
        let report = report();
        for name in [
            "echo-contracts",
            "echo-config",
            "echo-screen-contract",
            "echo-mcp-auth",
            "echo-mcp-client",
            "echo-agent-loop",
            "echo-wyoming",
            "echo-bridge",
        ] {
            let crates = [(name.into(), "xtask/".into())];
            assert!(
                evaluate(&report, &Changed::new(), &crates)
                    .unwrap_err()
                    .contains(&format!("{name} lines"))
            );
        }
        let absent = [("new".into(), "absent/".into())];
        assert!(
            evaluate(&report, &Changed::new(), &absent)
                .unwrap_err()
                .contains("missing executable")
        );
    }
}
