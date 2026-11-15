//! Small source lexer: syntax tokens retain their physical lines; literals are opaque.

#[derive(Debug, PartialEq)]
pub(super) struct Token<'a> {
    pub text: &'a str,
    pub line: usize,
}

pub(super) fn lex(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut line = 1;
    while cursor < bytes.len() {
        let start = cursor;
        let skipped = opaque_end(bytes, cursor);
        if let Some(end) = skipped {
            cursor = end;
        } else if bytes[cursor].is_ascii_alphabetic() || bytes[cursor] == b'_' {
            cursor += 1;
            while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_') {
                cursor += 1;
            }
            tokens.push(Token {
                text: &source[start..cursor],
                line,
            });
        } else {
            let width = source[cursor..].chars().next().map_or(1, char::len_utf8);
            cursor += width;
            if !bytes[start].is_ascii_whitespace() {
                tokens.push(Token {
                    text: &source[start..cursor],
                    line,
                });
            }
        }
        line += source[start..cursor]
            .chars()
            .filter(|character| *character == '\n')
            .count();
    }
    tokens
}

fn opaque_end(bytes: &[u8], start: usize) -> Option<usize> {
    let tail = &bytes[start..];
    if tail.starts_with(b"//") {
        return Some(start + tail.iter().position(|byte| *byte == b'\n').unwrap_or(tail.len()));
    }
    if tail.starts_with(b"/*") {
        return Some(block_end(bytes, start));
    }
    let prefix = usize::from(tail.starts_with(b"b") || tail.starts_with(b"c"));
    let quote = start + prefix;
    if bytes.get(quote) == Some(&b'"') {
        return Some(quoted_end(bytes, quote, b'"'));
    }
    if bytes.get(quote) == Some(&b'\'') {
        let end = quoted_end(bytes, quote, b'\'');
        let content = bytes.get(quote + 1..end.saturating_sub(1)).unwrap_or_default();
        // A lifetime has no closing quote; character literals have one scalar or an escape.
        if bytes.get(end.saturating_sub(1)) == Some(&b'\'')
            && (content.starts_with(b"\\") || std::str::from_utf8(content).is_ok_and(|s| s.chars().count() == 1))
        {
            return Some(end);
        }
    }
    if bytes.get(quote) == Some(&b'r') {
        let hashes = bytes[quote + 1..].iter().take_while(|byte| **byte == b'#').count();
        let opening = quote + 1 + hashes;
        if bytes.get(opening) == Some(&b'"') {
            let closing = [vec![b'"'], vec![b'#'; hashes]].concat();
            let offset = bytes[opening + 1..]
                .windows(closing.len())
                .position(|window| window == closing);
            return Some(offset.map_or(bytes.len(), |offset| opening + 1 + offset + closing.len()));
        }
    }
    None
}

fn quoted_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            value if value == quote => return cursor + 1,
            _ => cursor += 1,
        }
    }
    bytes.len()
}

fn block_end(bytes: &[u8], start: usize) -> usize {
    let mut cursor = start + 2;
    let mut depth = 1;
    while cursor < bytes.len() && depth > 0 {
        if bytes[cursor..].starts_with(b"/*") {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor..].starts_with(b"*/") {
            depth -= 1;
            cursor += 2;
        } else {
            cursor += 1;
        }
    }
    cursor
}
