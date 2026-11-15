//! Select test regions and follow Rust's external test module/helper declarations.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use super::tokens::{self, Kind, Token};

pub(super) struct Selection {
    pub ranges: Vec<(usize, usize)>,
    pub helpers: Vec<String>,
}

pub(super) fn inventory(sources: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut selected: BTreeSet<_> = sources.keys().filter(|path| integration(path)).cloned().collect();
    loop {
        let mut added = Vec::new();
        for (path, source) in sources {
            added.extend(select(path, source, selected.contains(path)).helpers);
        }
        let before = selected.len();
        selected.extend(added.into_iter().filter(|path| sources.contains_key(path)));
        if before == selected.len() {
            return selected;
        }
    }
}

pub(super) fn integration(path: &str) -> bool {
    path.split('/').any(|part| part == "tests")
}

pub(super) fn select(path: &str, source: &str, whole: bool) -> Selection {
    let tokens: Vec<_> = tokens::all(source)
        .into_iter()
        .filter(|t| !matches!(t.kind, Kind::Comment | Kind::Other))
        .collect();
    let whole = whole || (tokens.get(1).is_some_and(|t| t.text == "!") && attributes(&tokens, 0).1);
    let parent = Path::new(path).parent().unwrap_or(Path::new(""));
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let base = if matches!(stem, "lib" | "main" | "mod") {
        parent.to_owned()
    } else {
        parent.join(stem)
    };
    let mut result = Selection {
        ranges: Vec::new(),
        helpers: Vec::new(),
    };
    if whole {
        result.ranges.push((0, usize::MAX));
    }
    walk(&tokens, whole, &base, parent, &mut result);
    result
}

fn walk(tokens: &[Token<'_>], whole: bool, base: &Path, parent: &Path, out: &mut Selection) {
    let mut cursor = 0;
    while cursor < tokens.len() {
        let start = cursor;
        let (next, test, explicit) = attributes(tokens, cursor);
        cursor = next;
        let active = whole || test;
        if cursor >= tokens.len() {
            break;
        }
        if tokens[cursor].text == "pub" {
            cursor += 1;
            if tokens.get(cursor).is_some_and(|t| t.text == "(") {
                cursor = closing(tokens, cursor) + 1;
            }
            if cursor >= tokens.len() {
                break;
            }
        }
        if let Some(end) = module(tokens, cursor, active, explicit.as_deref(), base, parent, out) {
            cursor = end + 1;
            continue;
        }
        if test {
            let end = item_end(tokens, cursor);
            out.ranges
                .push((tokens[start].offset, tokens[end].offset + tokens[end].text.len()));
            if let Some(open) = (cursor..end).find(|i| tokens[*i].text == "{") {
                walk(&tokens[open + 1..end], true, base, parent, out);
            }
        }
        if active {
            include(tokens, cursor, parent, out);
        }
        if tokens[cursor].text == "{" {
            let end = closing(tokens, cursor);
            if cursor < end {
                walk(&tokens[cursor + 1..end], active, base, parent, out);
            }
            cursor = end + 1;
        } else {
            cursor += 1;
        }
    }
}

fn module(
    tokens: &[Token<'_>],
    cursor: usize,
    active: bool,
    explicit: Option<&str>,
    base: &Path,
    parent: &Path,
    out: &mut Selection,
) -> Option<usize> {
    if tokens[cursor].text != "mod" {
        return None;
    }
    let name = tokens.get(cursor + 1)?.text;
    let location = explicit.map_or_else(|| base.join(name), |p| parent.join(p));
    if tokens.get(cursor + 2).is_some_and(|t| t.text == ";") && active {
        if explicit.is_some() {
            out.helpers.push(normalize(&location));
        } else {
            out.helpers.push(normalize(&location.with_extension("rs")));
            out.helpers.push(normalize(&location.join("mod.rs")));
        }
    }
    if tokens.get(cursor + 2).is_none_or(|t| t.text != "{") {
        return None;
    }
    let end = closing(tokens, cursor + 2);
    if active {
        out.ranges
            .push((tokens[cursor].offset, tokens[end].offset + tokens[end].text.len()));
    }
    if cursor + 3 <= end {
        walk(&tokens[cursor + 3..end], active, &location, &location, out);
    }
    Some(end)
}

fn include(tokens: &[Token<'_>], cursor: usize, parent: &Path, out: &mut Selection) {
    if tokens[cursor].text == "include"
        && tokens.get(cursor + 1).is_some_and(|t| t.text == "!")
        && let Some(token) = tokens.get(cursor + 3).filter(|t| t.kind == Kind::String)
    {
        let value = super::literals::decode(token).0;
        if Path::new(&value).extension().is_some_and(|ext| ext == "rs") {
            out.helpers.push(normalize(&parent.join(value)));
        }
    }
}

fn attributes(tokens: &[Token<'_>], mut cursor: usize) -> (usize, bool, Option<String>) {
    let mut test = false;
    let mut path = None;
    while tokens.get(cursor).is_some_and(|t| t.text == "#") {
        let open = cursor + 1 + usize::from(tokens.get(cursor + 1).is_some_and(|t| t.text == "!"));
        if tokens.get(open).is_none_or(|t| t.text != "[") {
            break;
        }
        let end = closing(tokens, open);
        let attr = &tokens[open + 1..end];
        test |= attr.last().is_some_and(|t| t.text == "test")
            || (attr.first().is_some_and(|t| t.text == "cfg") && attr.iter().any(|t| t.text == "test"));
        if attr.first().is_some_and(|t| t.text == "path") {
            path = attr
                .iter()
                .find(|t| t.kind == Kind::String)
                .map(|t| super::literals::decode(t).0);
        }
        cursor = end + 1;
    }
    (cursor, test, path)
}

fn item_end(tokens: &[Token<'_>], start: usize) -> usize {
    for (i, t) in tokens.iter().enumerate().skip(start) {
        if t.text == "{" {
            return closing(tokens, i);
        }
        if t.text == ";" {
            return i;
        }
    }
    tokens.len().saturating_sub(1)
}

fn closing(tokens: &[Token<'_>], start: usize) -> usize {
    let close = match tokens[start].text {
        "{" => "}",
        "[" => "]",
        _ => ")",
    };
    let mut depth = 0_usize;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        if token.kind != Kind::Syntax {
            continue;
        }
        if token.text == tokens[start].text {
            depth += 1;
        }
        if token.text == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i;
            }
        }
    }
    tokens.len().saturating_sub(1)
}

fn normalize(path: &Path) -> String {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}
