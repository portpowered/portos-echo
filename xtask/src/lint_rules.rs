use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

type Names = BTreeSet<String>;

pub fn run() -> Result<(), String> {
    let base = git(&["merge-base", "origin/main", "HEAD"])?;
    let base = base.trim();
    let tree = git(&["ls-tree", "--name-only", base, "--", "Cargo.lock"])?;
    let old = if tree.trim().is_empty() {
        Names::new()
    } else {
        package_names(&git(&["show", &format!("{base}:Cargo.lock")])?)?
    };
    let current =
        fs::read_to_string("Cargo.lock").map_err(|error| format!("Cargo.lock: {error}"))?;
    let current = package_names(&current)?;
    let adrs = read_adrs()?;
    let missing = missing_names(&old, &current, &adrs);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing
            .iter()
            .map(|name| diagnostic(name))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("git output: {error}"))
}

fn read_adrs() -> Result<Vec<String>, String> {
    let entries = fs::read_dir("docs/adr").map_err(|error| format!("docs/adr/: {error}"))?;
    let mut contents = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| format!("docs/adr/: {error}"))?.path();
        if path.extension().is_some_and(|extension| extension == "md") {
            contents.push(
                fs::read_to_string(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?,
            );
        }
    }
    Ok(contents)
}

// Read the package-name subset of Cargo-generated lockfiles, not arbitrary TOML.
// All package headers and names are checked so corrupt records cannot disappear.
fn package_names(lock: &str) -> Result<Names, String> {
    let mut names = Names::new();
    let mut in_package = false;
    let mut name = None;
    for (index, raw) in lock.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') {
            if in_package {
                finish_package(&mut names, name.take())?;
            }
            if !line.ends_with(']') || (line.starts_with("[[") && !line.ends_with("]]")) {
                return Err(format!("Cargo.lock:{}: malformed table header", index + 1));
            }
            in_package = line == "[[package]]";
            if line.contains("package") && !in_package {
                return Err(format!(
                    "Cargo.lock:{}: malformed package header",
                    index + 1
                ));
            }
        } else if in_package
            && line
                .split('=')
                .next()
                .is_some_and(|key| key.trim() == "name")
        {
            let value = line
                .split_once('=')
                .map(|(_, value)| value.trim())
                .unwrap_or_default();
            let parsed = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'));
            let parsed = parsed
                .filter(|value| !value.is_empty() && value.chars().all(package_char))
                .ok_or_else(|| format!("Cargo.lock:{}: invalid package name", index + 1))?;
            if name.replace(parsed.to_owned()).is_some() {
                return Err(format!("Cargo.lock:{}: duplicate package name", index + 1));
            }
        }
    }
    if in_package {
        finish_package(&mut names, name)?;
    }
    Ok(names)
}

fn finish_package(names: &mut Names, name: Option<String>) -> Result<(), String> {
    names.insert(name.ok_or("Cargo.lock: package record missing name")?);
    Ok(())
}

fn package_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

fn missing_names(old: &Names, current: &Names, adrs: &[String]) -> Vec<String> {
    current
        .difference(old)
        .filter(|name| {
            !adrs.iter().any(|adr| {
                adr.split(package_char_inverse)
                    .any(|token| token == name.as_str())
            })
        })
        .cloned()
        .collect()
}

fn package_char_inverse(character: char) -> bool {
    !package_char(character)
}

fn diagnostic(name: &str) -> String {
    format!("Cargo.lock: {name}: no ADR in docs/adr/ names it")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Names {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn added_name_requires_exact_diagnostic() {
        let missing = missing_names(&names(&["alpha"]), &names(&["alpha", "beta"]), &[]);
        assert_eq!(missing, ["beta"]);
        assert_eq!(
            diagnostic(&missing[0]),
            "Cargo.lock: beta: no ADR in docs/adr/ names it"
        );
    }

    #[test]
    fn versions_duplicates_and_removals_do_not_add_names() {
        let old = package_names("[[package]]\nname = \"alpha\"\nversion = \"1\"\n").unwrap();
        let current = package_names("[[package]]\nname = \"alpha\"\nversion = \"2\"\n[[package]]\nname = \"alpha\"\nversion = \"3\"\n").unwrap();
        assert_eq!(current, names(&["alpha"]));
        assert_eq!(missing_names(&old, &current, &[]), Vec::<String>::new());
        assert_eq!(
            missing_names(&old, &Names::new(), &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn absent_base_and_empty_current() {
        assert_eq!(
            missing_names(&Names::new(), &names(&["beta", "alpha"]), &[]),
            ["alpha", "beta"]
        );
        assert_eq!(
            package_names("# empty\nversion = 4\n").unwrap(),
            Names::new()
        );
    }

    #[test]
    fn cargo_generated_fields_are_not_packages() {
        let lock = "# Generated\r\nversion = 4\r\n[[package]]\r\nname = \"alpha\" # name\r\nversion = \"1.0\"\nsource = \"registry+url\"\nchecksum = \"abc\"\ndependencies = [\n \"beta\",\n]\n[metadata]\nname = \"metadata\"\n";
        assert_eq!(package_names(lock).unwrap(), names(&["alpha"]));
    }

    #[test]
    fn malformed_records_fail_closed() {
        for lock in [
            "[[package]]\nversion = \"1\"",
            "[[package]]\nname = beta",
            "[[package]]\nname = \"beta",
            "[[package]\nname = \"beta\"",
            "[package]\nname = \"beta\"",
            "[[package]]\nname = \"a\"\nname = \"b\"",
        ] {
            assert!(package_names(lock).is_err(), "accepted {lock}");
        }
    }

    #[test]
    fn exact_tokens_across_adrs_cover_added_names() {
        let adrs = vec!["Use `alpha`.".into(), "Packages:\n- beta; chosen.".into()];
        assert_eq!(
            missing_names(&Names::new(), &names(&["alpha", "beta"]), &adrs),
            Vec::<String>::new()
        );
    }

    #[test]
    fn substring_case_and_longer_names_do_not_cover_package() {
        for adr in ["alphabetabeta", "beta-extra", "BETA", "beta_suffix", ""] {
            assert_eq!(
                missing_names(&Names::new(), &names(&["beta"]), &[adr.into()]),
                ["beta"]
            );
        }
    }

    #[test]
    fn missing_names_are_distinct_and_sorted() {
        let current = package_names("[[package]]\nname = \"zeta\"\n[[package]]\nname = \"beta\"\n[[package]]\nname = \"zeta\"").unwrap();
        assert_eq!(
            missing_names(&Names::new(), &current, &[]),
            ["beta", "zeta"]
        );
    }
}
