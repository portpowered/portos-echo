//! Deliberately small CSS grammar: documented selectors and inert declarations.
//! Reject escapes/comments instead of trying to normalize browser CSS parsing.
use super::{Error, extension, relative};
use std::collections::BTreeMap;

pub(super) fn validate(css: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    if !css.is_ascii() || css.contains(['\\', '@', '<', '>']) || css.contains("/*") || css.contains("*/") {
        return Err(Error::Css);
    }
    let mut rest = css.trim();
    while !rest.is_empty() {
        let (selectors, body) = rest.split_once('{').ok_or(Error::Css)?;
        let (declarations, tail) = body.split_once('}').ok_or(Error::Css)?;
        for selector in selectors.split(',') {
            if ![
                ".pack-clock",
                ".pack-date",
                ".pack-status",
                ".pack-link-code",
                ".pack-approval",
                ".pack-error",
                ".pack-stage",
            ]
            .contains(&selector.trim())
            {
                return Err(Error::Css);
            }
        }
        for declaration in declarations.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, value) = declaration.split_once(':').ok_or(Error::Css)?;
            declaration_value(key.trim(), value.trim(), files)?;
        }
        rest = tail.trim();
    }
    Ok(())
}

fn declaration_value(key: &str, value: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::Css);
    }
    match key {
        "color" | "background-color" | "border-color" => {
            if value.len() != 7 || !value.starts_with('#') || !value[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Css);
            }
        }
        "background-image" => image(value, files)?,
        "font-weight" if ["normal", "bold", "400", "500", "600", "700"].contains(&value) => (),
        "text-align" if ["left", "center", "right"].contains(&value) => (),
        "opacity" => number(value, 0.0, 1.0)?,
        "font-size" | "border-radius" | "border-width" | "padding" | "gap" => dimension(value)?,
        _ => return Err(Error::Css),
    }
    Ok(())
}

fn number(value: &str, minimum: f64, maximum: f64) -> Result<(), Error> {
    let parsed = value.parse::<f64>().map_err(|_| Error::Css)?;
    if !parsed.is_finite() || !(minimum..=maximum).contains(&parsed) {
        return Err(Error::Css);
    }
    Ok(())
}

fn dimension(value: &str) -> Result<(), Error> {
    let raw = ["px", "rem", "em", "%"]
        .into_iter()
        .find_map(|unit| value.strip_suffix(unit))
        .ok_or(Error::Css)?;
    number(raw, 0.0, 100.0)
}

fn image(value: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    let value = value
        .strip_prefix("url(")
        .and_then(|v| v.strip_suffix(')'))
        .ok_or(Error::Css)?
        .trim();
    let path = if value.starts_with('"') {
        value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or(Error::Css)?
    } else if value.starts_with('\'') {
        value
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .ok_or(Error::Css)?
    } else {
        value
    };
    relative(path).map_err(|_| Error::Css)?;
    if !files.contains_key(path) || !["png", "webp"].contains(&extension(path).map_err(|_| Error::Css)?) {
        return Err(Error::Css);
    }
    Ok(())
}

/// Only called on the exact bytes admitted by `validate`. References are rooted
/// at the pack, including when the stylesheet itself is in a nested directory.
pub(super) fn render(css: &str, base: &str, query: &str) -> Result<String, Error> {
    let mut output = String::new();
    let mut rest = css.trim();
    while !rest.is_empty() {
        let (selectors, body) = rest.split_once('{').ok_or(Error::Css)?;
        let (declarations, tail) = body.split_once('}').ok_or(Error::Css)?;
        output.push_str(
            &selectors
                .split(',')
                .map(|s| format!(".screen-pack {}", s.trim()))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push('{');
        for declaration in declarations.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, value) = declaration.split_once(':').ok_or(Error::Css)?;
            output.push_str(key.trim());
            output.push(':');
            if key.trim() == "background-image" {
                let path = value
                    .trim()
                    .strip_prefix("url(")
                    .and_then(|v| v.strip_suffix(')'))
                    .ok_or(Error::Css)?;
                let path = path.trim().trim_matches(['\'', '"']);
                output.push_str(&format!("url(\"{base}{path}?{query}\")"));
            } else {
                output.push_str(value.trim());
            }
            output.push(';');
        }
        output.push('}');
        rest = tail.trim();
    }
    Ok(output)
}
