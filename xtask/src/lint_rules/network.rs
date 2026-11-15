use super::{
    literals, test_source,
    tokens::{self, Kind},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    process::Command,
};

pub(super) fn run() -> Result<(), String> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*.rs", ":(exclude)factory/"])
        .output()
        .map_err(|e| format!(".:1: network-tag input: {e}"))?;
    if !output.status.success() {
        return Err(".:1: network-tag input: git ls-files failed".into());
    }
    let paths = String::from_utf8(output.stdout).map_err(|e| format!(".:1: network-tag input: {e}"))?;
    let mut sources = BTreeMap::new();
    for path in paths.split('\0').filter(|p| !p.is_empty()) {
        let source = fs::read_to_string(path).map_err(|e| format!("{path}:1: network-tag input: {e}"))?;
        sources.insert(path.to_owned(), source);
    }
    let (errors, count) = check(&sources);
    println!("lint-rules: literal exemptions: {count}");
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn check(sources: &BTreeMap<String, String>) -> (Vec<String>, usize) {
    let whole = test_source::inventory(sources);
    let mut errors = Vec::new();
    let mut count = 0;
    for (path, source) in sources {
        let cloud = path.rsplit_once('/').is_some_and(|(parent, name)| {
            parent.ends_with("tests") && parent.split('/').next_back() == Some("tests") && name.starts_with("cloud_")
        }) && source.lines().next() == Some("#![cfg(feature = \"cloud-tests\")]");
        if cloud {
            continue;
        }
        let selection = test_source::select(path, source, whole.contains(path));
        let all = tokens::all(source);
        let annotations: BTreeSet<_> = all
            .iter()
            .filter(|t| t.kind == Kind::Comment)
            .filter(|t| {
                t.text
                    .strip_prefix("// lint-rules: allow-literal ")
                    .is_some_and(|reason| !reason.trim().is_empty())
            })
            .map(|t| t.line)
            .collect();
        let mut exempted = BTreeSet::new();
        let mut violations = BTreeSet::new();
        for token in all.iter().filter(|t| t.kind == Kind::String) {
            if !selection
                .ranges
                .iter()
                .any(|(start, end)| *start <= token.offset && token.offset < *end)
            {
                continue;
            }
            let (value, lines) = literals::decode(token);
            for line in literals::remote_lines(&value, &lines) {
                if annotations.contains(&line) {
                    exempted.insert(line);
                } else {
                    violations.insert(line);
                }
            }
        }
        count += exempted.len();
        errors.extend(violations.into_iter().map(|line| format!("{path}:{line}: network-tag: non-loopback test literal requires cloud gate or reasoned inert-literal annotation")));
    }
    (errors, count)
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
