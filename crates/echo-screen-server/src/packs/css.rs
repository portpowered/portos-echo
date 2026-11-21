//! Deliberately small CSS grammar: documented selectors and inert declarations.
//! Reject escapes/comments instead of trying to normalize browser CSS parsing.
use super::{Error, extension, relative};
use std::{collections::BTreeMap, fmt::Write};

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
        "text-shadow" => shadows(value)?,
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

/// One or two shadows: `<x>px <y>px [<blur>px] <colour>`, lengths within 16 px,
/// the colour `#rrggbb` or `rgba(r, g, b, a)`.
fn shadows(value: &str) -> Result<(), Error> {
    // The only commas outside a colour separate the shadows.
    let (mut parts, mut start, mut inside) = (Vec::new(), 0, false);
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'(' if !inside => inside = true,
            b')' if inside => inside = false,
            b'(' | b')' => return Err(Error::Css),
            b',' if !inside => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => (),
        }
    }
    parts.push(&value[start..]);
    if inside || parts.len() > 2 {
        return Err(Error::Css);
    }
    for part in parts {
        let part = part.trim();
        let (lengths, colour) = match part.find("rgba(") {
            Some(index) => part.split_at(index),
            None => part.rsplit_once(' ').ok_or(Error::Css)?,
        };
        shadow_colour(colour.trim())?;
        let lengths = lengths.split_ascii_whitespace().collect::<Vec<_>>();
        if !(2..=3).contains(&lengths.len()) {
            return Err(Error::Css);
        }
        for (index, length) in lengths.into_iter().enumerate() {
            let raw = length.strip_suffix("px").ok_or(Error::Css)?;
            // Offsets may point either way; a blur radius is never negative.
            number(raw, if index < 2 { -16.0 } else { 0.0 }, 16.0)?;
        }
    }
    Ok(())
}

fn shadow_colour(value: &str) -> Result<(), Error> {
    if let Some(channels) = value.strip_prefix("rgba(").and_then(|v| v.strip_suffix(')')) {
        let channels = channels.split(',').map(str::trim).collect::<Vec<_>>();
        if channels.len() != 4 {
            return Err(Error::Css);
        }
        for channel in &channels[..3] {
            if !channel.bytes().all(|b| b.is_ascii_digit()) || channel.parse::<u8>().is_err() {
                return Err(Error::Css);
            }
        }
        return number(channels[3], 0.0, 1.0);
    }
    if value.len() != 7 || !value.starts_with('#') || !value[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Css);
    }
    Ok(())
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
                write!(output, "url(\"{base}{path}?{query}\")").map_err(|_| Error::Css)?;
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
