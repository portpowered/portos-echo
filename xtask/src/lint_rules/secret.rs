use std::{fs, process::Command};

#[path = "tokens.rs"]
mod tokens;
use tokens::Token;

pub(super) fn run() -> Result<(), String> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "crates/**/*.rs"])
        .output()
        .map_err(|error| format!("crates/:1: secret-format input: {error}"))?;
    if !output.status.success() {
        return Err("crates/:1: secret-format input: git ls-files failed".into());
    }
    let paths = String::from_utf8(output.stdout).map_err(|error| format!("crates/:1: secret-format input: {error}"))?;
    let mut errors = Vec::new();
    for path in paths.split('\0').filter(|path| !path.is_empty()) {
        let source = fs::read_to_string(path).map_err(|error| format!("{path}:1: secret-format input: {error}"))?;
        errors.extend(check(path, &source));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn check(path: &str, source: &str) -> Vec<String> {
    let tokens = tokens::lex(source);
    let mut errors = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let violation = match token.text {
            "impl" => formatting_impl(&tokens[index + 1..]),
            "#" => debug_struct(&tokens[index + 1..]),
            _ => false,
        };
        if violation {
            errors.push(format!(
                "{path}:{}: secret-format: Secret must not implement Debug or Display or derive Debug",
                token.line
            ));
        }
    }
    errors
}

fn formatting_impl(tokens: &[Token<'_>]) -> bool {
    let end = tokens
        .iter()
        .position(|token| matches!(token.text, "{" | ";"))
        .unwrap_or(tokens.len());
    let header = &tokens[..end];
    let mut depth = 0_usize;
    for (index, token) in header.iter().enumerate() {
        match token.text {
            "<" => depth += 1,
            ">" => depth = depth.saturating_sub(1),
            "for" if depth == 0 => {
                let trait_start = if header.first().is_some_and(|token| token.text == "<") {
                    balanced_end(header, 0, "<", ">") + 1
                } else {
                    0
                };
                let trait_name = path_name(&header[trait_start.min(index)..index]);
                return matches!(trait_name, Some("Debug" | "Display"))
                    && path_name(&header[index + 1..]) == Some("Secret");
            }
            _ => {}
        }
    }
    false
}

fn path_name<'a>(tokens: &'a [Token<'_>]) -> Option<&'a str> {
    let mut name = None;
    for token in tokens {
        if matches!(token.text, "<" | "where") {
            break;
        }
        if token.text != ":" {
            name = Some(token.text);
        }
    }
    name
}

fn balanced_end(tokens: &[Token<'_>], start: usize, open: &str, close: &str) -> usize {
    let mut depth = 0_usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if token.text == open {
            depth += 1;
        }
        if token.text == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return index;
            }
        }
    }
    tokens.len()
}

fn debug_struct(tokens: &[Token<'_>]) -> bool {
    if tokens.first().is_none_or(|token| token.text != "[") {
        return false;
    }
    let end = balanced_end(tokens, 0, "[", "]");
    let attribute = &tokens[..end];
    if attribute.get(1).is_none_or(|token| token.text != "derive")
        || !attribute.iter().any(|token| token.text == "Debug")
    {
        return false;
    }
    let mut cursor = end + 1;
    while let Some(token) = tokens.get(cursor) {
        match token.text {
            "#" if tokens.get(cursor + 1).is_some_and(|token| token.text == "[") => {
                cursor = balanced_end(tokens, cursor + 1, "[", "]") + 1;
            }
            "pub" => {
                cursor += 1;
                if tokens.get(cursor).is_some_and(|token| token.text == "(") {
                    cursor = balanced_end(tokens, cursor, "(", ")") + 1;
                }
            }
            "struct" => return secret_struct(&tokens[cursor + 1..]),
            _ => return false,
        }
    }
    false
}

fn secret_struct(tokens: &[Token<'_>]) -> bool {
    if tokens.first().is_some_and(|token| token.text == "Secret") {
        return true;
    }
    let mut cursor = 1;
    if tokens.get(cursor).is_some_and(|token| token.text == "<") {
        cursor = balanced_end(tokens, cursor, "<", ">") + 1;
    }
    while let Some(token) = tokens.get(cursor) {
        match token.text {
            ";" => return false,
            "{" | "(" => {
                let close = if token.text == "{" { "}" } else { ")" };
                let end = balanced_end(tokens, cursor, token.text, close);
                return tokens[cursor + 1..end]
                    .windows(2)
                    .any(|pair| pair[0].text == "Secret" && pair[1].text == "<");
            }
            _ => cursor += 1,
        }
    }
    false
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
