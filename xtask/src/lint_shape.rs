use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

#[path = "shape_json.rs"]
mod shape_json;

// docs/program/decisions.md: coordinator lint-domain decision, 2026-11-15.
const VENDORED_ROOTS: [&str; 1] = ["factory/"];
type Members = BTreeMap<String, String>;

pub fn header() -> String {
    format!("lint-shape: vendored roots excluded: {}", VENDORED_ROOTS.join(", "))
}

fn first_party(path: &str) -> bool {
    !VENDORED_ROOTS.iter().any(|root| path.starts_with(root))
}

fn single_line(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn diagnostic(path: &str, rule: &str, detail: &str) -> String {
    format!("{}: {rule}: {}", single_line(path), single_line(detail))
}

pub fn outcome(result: Result<Vec<String>, String>) -> (u8, Vec<String>) {
    match result {
        Ok(lines) => (u8::from(!lines.is_empty()), lines),
        Err(message) => (1, vec![format!("lint-shape: input: {}", single_line(&message))]),
    }
}

fn command_result(label: &str, result: Result<(bool, Vec<u8>), String>) -> Result<String, String> {
    let (success, bytes) = result.map_err(|e| format!("cannot run {label}: {e}"))?;
    if !success {
        return Err(format!("{label} failed; run it directly for details"));
    }
    String::from_utf8(bytes).map_err(|_| format!("{label} returned non-UTF-8 output"))
}

fn command(program: &str, args: &[&str]) -> Result<String, String> {
    let result = Command::new(program)
        .args(args)
        .output()
        .map(|o| (o.status.success(), o.stdout))
        .map_err(|e| e.to_string());
    command_result(&format!("{program} {}", args.join(" ")), result)
}

fn read_result(path: &str, result: Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    result.map_err(|e| format!("cannot read {}: {e}", single_line(path)))
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    let root = std::fs::canonicalize(".").map_err(|_| "cannot resolve repository root")?;
    let resolved =
        std::fs::canonicalize(path).map_err(|_| format!("cannot resolve required file {}", single_line(path)))?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "required file {} resolves outside repository",
            single_line(path)
        ));
    }
    read_result(path, std::fs::read(path).map_err(|e| e.to_string()))
}

// Lexical normalization works for Cargo's absolute Windows paths on either host.
fn normalize(path: &str) -> Result<String, String> {
    let path = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("path escapes its root".into());
                }
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn relative(path: &str) -> Result<String, String> {
    if path.starts_with(['/', '\\']) || path.contains(':') {
        return Err("expected a repository-relative path".into());
    }
    let normalized = normalize(path)?;
    if normalized.is_empty() {
        return Err("empty repository path".into());
    }
    Ok(normalized)
}

fn lanes(text: &str) -> Result<Members, String> {
    let value = toml::from_str::<toml::Value>(text).map_err(|_| "lanes.toml: malformed TOML")?;
    let entries = value
        .get("crate")
        .and_then(toml::Value::as_array)
        .ok_or("lanes.toml: expected [[crate]] entries")?;
    let mut members = Members::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("lanes.toml: crate requires name")?;
        let path = entry
            .get("path")
            .and_then(toml::Value::as_str)
            .ok_or("lanes.toml: crate requires path")?;
        let path = relative(path).map_err(|e| format!("lanes.toml: {e}"))?;
        if members.insert(name.into(), path).is_some() {
            return Err(format!("lanes.toml: duplicate crate {}", single_line(name)));
        }
    }
    Ok(members)
}

fn tracked(text: &str) -> Result<Vec<String>, String> {
    if !text.is_empty() && !text.ends_with('\0') {
        return Err("git ls-files: expected NUL-terminated paths".into());
    }
    text.split_terminator('\0')
        .map(|p| relative(p).map_err(|e| format!("git ls-files: {e}")))
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|s| s.into_iter().collect())
}

fn source(path: &str) -> bool {
    ["rs", "ts", "sh", "py"]
        .iter()
        .any(|ext| path.ends_with(&format!(".{ext}")))
}

fn check_members(actual: &Members, declared: &Members) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, path) in actual.iter().filter(|(_, p)| first_party(&format!("{p}/"))) {
        let manifest = format!("{path}/Cargo.toml");
        match declared.get(name) {
            None => lines.push(diagnostic(
                &manifest,
                "crate-set",
                &format!("workspace member {name} is not declared in lanes.toml"),
            )),
            Some(expected) if expected != path => lines.push(diagnostic(
                &manifest,
                "crate-set",
                &format!("workspace member {name} directory {path} differs from lanes.toml {expected}"),
            )),
            _ => {}
        }
        if name != "xtask" && !name.starts_with("echo-") {
            lines.push(diagnostic(
                &manifest,
                "crate-name",
                &format!("{name} must start with echo- (except xtask)"),
            ));
        }
    }
    for (name, path) in declared.iter().filter(|(_, p)| first_party(&format!("{p}/"))) {
        if !actual.contains_key(name) {
            lines.push(diagnostic(
                "lanes.toml",
                "crate-set",
                &format!("declared crate {name} at {path} is not a workspace member"),
            ));
        }
    }
    lines
}

fn check_files<F>(paths: &[String], mut read: F) -> Result<Vec<String>, String>
where
    F: FnMut(&str) -> Result<Vec<u8>, String>,
{
    let mut lines = Vec::new();
    let mut directories = BTreeMap::<&str, usize>::new();
    for path in paths.iter().filter(|p| first_party(p)) {
        if source(path) {
            let bytes = read(path)?;
            let count = bytes.split_inclusive(|&byte| byte == b'\n').count();
            if count > 500 {
                lines.push(diagnostic(path, "file-lines", &format!("{count} lines exceeds 500")));
            }
        }
        if let Some((parent, _)) = path.rsplit_once('/')
            && ["crates/", "xtask/", "web/"].iter().any(|root| path.starts_with(root))
        {
            *directories.entry(parent).or_default() += 1;
        }
    }
    for (directory, count) in directories {
        if count > 10 {
            lines.push(diagnostic(
                directory,
                "module-files",
                &format!("{count} direct files exceeds 10"),
            ));
        }
    }
    Ok(lines)
}

pub fn run() -> Result<Vec<String>, String> {
    let paths = tracked(&command("git", &["ls-files", "-z"])?)?;
    let metadata = command(
        "cargo",
        &[
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
            "--locked",
        ],
    )?;
    let actual = shape_json::members(&metadata)?;
    let bytes = read("lanes.toml")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "lanes.toml: invalid UTF-8")?;
    let declared = lanes(text)?;
    let mut lines = check_members(&actual, &declared);
    lines.extend(check_files(&paths, read)?);
    lines.sort();
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(entries: &[(&str, &str)]) -> Members {
        entries.iter().map(|(n, p)| (n.to_string(), p.to_string())).collect()
    }

    #[test]
    fn crate_sets_and_directories() {
        let actual = members(&[("xtask", "xtask"), ("echo-new", "crates/new")]);
        assert_eq!(check_members(&actual, &actual), Vec::<String>::new());
        assert_eq!(
            check_members(&actual, &members(&[("xtask", "xtask")])),
            ["crates/new/Cargo.toml: crate-set: workspace member echo-new is not declared in lanes.toml"]
        );
        assert_eq!(
            check_members(&members(&[("xtask", "xtask")]), &actual),
            ["lanes.toml: crate-set: declared crate echo-new at crates/new is not a workspace member"]
        );
        assert_eq!(
            check_members(&actual, &members(&[("xtask", "tools"), ("echo-new", "crates/new")])),
            ["xtask/Cargo.toml: crate-set: workspace member xtask directory xtask differs from lanes.toml tools"]
        );
    }

    #[test]
    fn names_have_area_prefix() {
        let input = members(&[
            ("xtask", "xtask"),
            ("echo-example", "crates/example"),
            ("other", "crates/other"),
        ]);
        assert_eq!(
            check_members(&input, &input),
            ["crates/other/Cargo.toml: crate-name: other must start with echo- (except xtask)"]
        );
    }

    #[test]
    fn physical_line_boundaries_for_all_source_extensions() {
        for extension in ["rs", "ts", "sh", "py"] {
            let path = format!("scripts/example.{extension}");
            for separator in ["\n", "\r\n"] {
                for count in [0, 500, 501] {
                    let bytes = separator.repeat(count).into_bytes();
                    let got = check_files(std::slice::from_ref(&path), |_| Ok(bytes.clone())).unwrap();
                    let expected = if count == 501 {
                        vec![format!("{path}: file-lines: 501 lines exceeds 500")]
                    } else {
                        vec![]
                    };
                    assert_eq!(got, expected);
                }
            }
            let got = check_files(std::slice::from_ref(&path), |_| {
                Ok(format!("{}last", "\n".repeat(500)).into_bytes())
            })
            .unwrap();
            assert_eq!(got, [format!("{path}: file-lines: 501 lines exceeds 500")]);
        }
    }

    #[test]
    fn module_counts_include_tests_and_non_sources_but_not_nested_children() {
        for root in ["crates/example", "xtask", "web/screen"] {
            let mut paths: Vec<_> = (0..10).map(|i| format!("{root}/test{i}.txt")).collect();
            paths.push(format!("{root}/nested/one.txt"));
            assert_eq!(check_files(&paths, |_| unreachable!()).unwrap(), Vec::<String>::new());
            paths.push(format!("{root}/tests.rs"));
            assert_eq!(
                check_files(&paths, |_| Ok(vec![])).unwrap(),
                [format!("{root}: module-files: 11 direct files exceeds 10")]
            );
        }
        let paths: Vec<_> = (0..11).map(|i| format!("docs/{i}.txt")).collect();
        assert_eq!(check_files(&paths, |_| unreachable!()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn vendored_boundary_is_visible_and_exact() {
        assert_eq!(header(), "lint-shape: vendored roots excluded: factory/");
        let paths = tracked("factory/example.py\0scripts/example.py\0factory-extra/example.py\0").unwrap();
        let got = check_files(&paths, |p| {
            assert_ne!(p, "factory/example.py");
            Ok(b"\n".repeat(501))
        })
        .unwrap();
        assert_eq!(
            got,
            [
                "factory-extra/example.py: file-lines: 501 lines exceeds 500",
                "scripts/example.py: file-lines: 501 lines exceeds 500"
            ]
        );
        let vendor = members(&[("vendor", "factory/vendor")]);
        assert_eq!(check_members(&vendor, &Members::new()), Vec::<String>::new());
        assert_eq!(check_members(&Members::new(), &vendor), Vec::<String>::new());
    }

    #[test]
    fn tracked_paths_and_diagnostics_keep_identity_and_order() {
        let paths = tracked("z file.py\0a\nfile.rs\0z file.py\0").unwrap();
        assert_eq!(paths, ["a\nfile.rs", "z file.py"]);
        let mut got = check_members(&members(&[("wrong", "xtask/seed")]), &Members::new());
        got.extend(check_files(&paths, |_| Ok(b"\n".repeat(501))).unwrap());
        got.sort();
        assert_eq!(
            got,
            [
                "a\\nfile.rs: file-lines: 501 lines exceeds 500",
                "xtask/seed/Cargo.toml: crate-name: wrong must start with echo- (except xtask)",
                "xtask/seed/Cargo.toml: crate-set: workspace member wrong is not declared in lanes.toml",
                "z file.py: file-lines: 501 lines exceeds 500"
            ]
        );
        assert_eq!(tracked(""), Ok(vec![]));
        assert!(tracked("unterminated").is_err());
        for path in ["../escape.rs\0", "/absolute.rs\0", "C:\\absolute.rs\0"] {
            assert!(tracked(path).is_err());
        }
    }

    #[test]
    fn lanes_parse_normalize_and_reject_invalid_declarations() {
        let parsed = lanes("[[crate]]\nname='xtask'\npath='./tools/../xtask'\nkind='tool'").unwrap();
        assert_eq!(parsed, members(&[("xtask", "xtask")]));
        assert_eq!(normalize("C:\\repo\\.\\xtask"), Ok("C:/repo/xtask".into()));
        for input in [
            "bad = [",
            "",
            "[[crate]]\nname='xtask'",
            "[[crate]]\npath='xtask'",
            "[[crate]]\nname='a'\npath='../a'",
            "[[crate]]\nname='a'\npath='a'\n[[crate]]\nname='a'\npath='b'",
        ] {
            assert!(lanes(input).is_err(), "{input}");
        }
    }

    #[test]
    fn required_input_failures_are_actionable_and_status_one() {
        assert_eq!(outcome(Ok(vec![])), (0, vec![]));
        assert_eq!(outcome(Ok(vec!["violation".into()])), (1, vec!["violation".into()]));
        assert_eq!(
            outcome(command_result("git ls-files", Ok((false, vec![]))).map(|_| vec![])),
            (
                1,
                vec!["lint-shape: input: git ls-files failed; run it directly for details".into()]
            )
        );
        assert_eq!(
            command_result("cargo metadata", Err("not found".into())),
            Err("cannot run cargo metadata: not found".into())
        );
        assert_eq!(
            command_result("git", Ok((true, vec![255]))),
            Err("git returned non-UTF-8 output".into())
        );
        let got = check_files(&["missing.rs".into()], |p| read_result(p, Err("denied".into())));
        assert_eq!(
            outcome(got),
            (1, vec!["lint-shape: input: cannot read missing.rs: denied".into()])
        );
        assert_eq!(
            outcome(Err("bad\ninput".into())),
            (1, vec!["lint-shape: input: bad\\ninput".into()])
        );
    }
}
