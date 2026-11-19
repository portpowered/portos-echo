//! Static launch policy, deliberately bounded to literal launch commands/config.
use std::{fs, process::Command};

pub(super) fn run() -> Result<(), String> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", ":(exclude)factory/"])
        .output()
        .map_err(|error| format!(".:1: root-launch input: {error}"))?;
    if !output.status.success() {
        return Err(".:1: root-launch input: git ls-files failed".into());
    }
    let paths = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let mut errors = Vec::new();
    for path in paths.split('\0').filter(|path| !path.is_empty()) {
        if !std::path::Path::new(path).is_file() {
            continue;
        }
        let bytes = fs::read(path).map_err(|error| format!("{path}:1: {error}"))?;
        if let Ok(source) = std::str::from_utf8(&bytes) {
            errors.extend(check(path, source));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn forbidden(source: &str) -> bool {
    let normalized = source.replace("\\\n", " ").replace("\\\r\n", " ");
    let words: Vec<_> = normalized
        .split(|c: char| c.is_whitespace() || "\\\"'`,:=()[]{};".contains(c))
        .filter(|word| !word.is_empty())
        .collect();
    words.windows(2).enumerate().any(|(index, pair)| {
        let runas = pair[0] == "runas" || pair[0].ends_with("/runas");
        let root = matches!(pair[1], "root" | "0");
        runas
            && root
            && (words.contains(&"run_as")
                || words[index + 2..]
                    .iter()
                    .any(|word| matches!(word.rsplit('/').next(), Some("echo-bridge" | "codex-app-server"))))
    })
}

fn refusal_fixture(path: &str, source: &str, offset: usize, line: &str) -> bool {
    if !path.ends_with(".rs") || !line.contains("// lint-rules: root-refusal fixture") {
        return false;
    }
    let selected = super::test_source::select(path, source, false);
    let in_test = selected
        .ranges
        .iter()
        .any(|(start, end)| *start <= offset && offset < *end);
    let tokens = super::tokens::lex(&source[..offset]);
    let function = tokens.windows(2).rev().find(|pair| pair[0].text == "fn");
    in_test && function.is_some_and(|pair| pair[1].text.ends_with("_refuses_root"))
}

fn check(path: &str, source: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut logical = String::new();
    let mut first = 1;
    let mut offset = 0;
    for (index, line) in source.lines().enumerate() {
        if logical.is_empty() {
            first = index + 1;
        }
        logical.push_str(line);
        if line.trim_end().ends_with('\\') {
            logical.push('\n');
            continue;
        }
        if forbidden(&logical) && !refusal_fixture(path, source, offset, &logical) {
            errors.push(format!(
                "{path}:{first}: root launch of echo-bridge or codex-app-server is forbidden; use nobody 3990,inet"
            ));
        }
        offset += logical.len() + 1;
        logical.clear();
    }
    if forbidden(&logical) {
        errors.push(format!("{path}:{first}: root launch is forbidden"));
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_launches_and_launcher_prefixes_refuse_root() {
        for program in ["echo-bridge", "codex-app-server"] {
            for helper in ["runas", "/data/local/hassmic/runas"] {
                for user in ["root", "0"] {
                    for launch in [
                        format!("{helper} {user} 3990,inet /bin/{program}"),
                        format!("'{helper}' \"{user}\" 3990,inet \\\n /bin/{program}"),
                        format!(r#""run_as": "{helper} {user} 3990,inet""#),
                    ] {
                        assert_eq!(check("scripts/selected.sh", &launch).len(), 1, "{launch}");
                    }
                }
            }
        }
    }

    #[test]
    fn only_narrow_negative_test_fixtures_are_exempt() {
        let user = "root";
        let launch = ["runas", user, "3990,inet", "echo-bridge"].join(" ");
        assert_eq!(check("tests/hidden.sh", &launch).len(), 1);
        let fixture = format!(
            "#[test]\nfn launcher_refuses_root() {{\n let input = \"{launch}\"; // lint-rules: root-refusal fixture\n}}\n"
        );
        assert!(check("fixture.rs", &fixture).is_empty());
        assert_eq!(
            check(
                "fixture.rs",
                &fixture.replace("launcher_refuses_root", "launcher_starts")
            )
            .len(),
            1
        );
        assert_eq!(check("fixture.rs", &fixture.replace("#[test]", "")).len(), 1);
        assert!(check("valid.sh", "runas nobody 3990,inet /bin/echo-bridge").is_empty());
        assert!(check("valid.sh", "runas root inet /bin/id").is_empty()); // lint-rules: root-refusal fixture
    }

    #[test]
    fn diagnostic_names_path_and_first_continuation_line_without_content() {
        let user = "root";
        let launch = ["runas", user, "3990,inet", "echo-bridge"].join(" ");
        let errors = check("selected.sh", &format!("# header\n{launch}"));
        assert_eq!(
            errors,
            ["selected.sh:2: root launch of echo-bridge or codex-app-server is forbidden; use nobody 3990,inet"]
        );
    }
}
