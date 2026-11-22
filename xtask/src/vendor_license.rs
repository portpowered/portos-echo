use std::{fs, path::Path};

pub(super) fn run() -> Result<(), String> {
    let root = Path::new("third_party");
    if !root.try_exists().map_err(|error| format!("vendor licences: {error}"))? {
        return Ok(());
    }
    let mut violations = Vec::new();
    scan(root, &mut violations).map_err(|error| format!("vendor licences: {error}"))?;
    violations.sort();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations.join("\n"))
    }
}

fn scan(directory: &Path, violations: &mut Vec<String>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if kind.is_dir() {
            scan(&path, violations)?;
        } else if kind.is_file() {
            let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            if forbidden(&String::from_utf8_lossy(&bytes).to_ascii_lowercase()) {
                violations.push(format!(
                    "{}: vendor licence: GNU GPL family is prohibited",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn forbidden(source: &str) -> bool {
    let titles = [
        "gnu general public license",
        "gnu lesser general public license",
        "gnu affero general public license",
        "gnu library general public license",
    ];
    titles.iter().any(|title| source.contains(title))
        || source.lines().any(|line| {
            let Some((_, expression)) = line.split_once("spdx-license-identifier:") else {
                return false;
            };
            expression
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .any(|word| {
                    ["gpl", "lgpl", "agpl"].iter().any(|family| {
                        word == *family || word.strip_prefix(family).is_some_and(|rest| rest.starts_with('-'))
                    })
                })
        })
}
