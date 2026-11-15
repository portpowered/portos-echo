//! Decode source literals without evaluation, resolution or network access.
use super::tokens::Token;
use std::collections::BTreeSet;

pub(super) fn decode(token: &Token<'_>) -> (String, Vec<usize>) {
    let quote = token.text.find('"').unwrap_or(0);
    let raw = token.text[..quote].contains('r');
    let tail = &token.text[quote + 1..];
    let end = if raw {
        tail.rfind('"').unwrap_or(tail.len())
    } else {
        tail.len().saturating_sub(1)
    };
    let mut chars = tail[..end].chars().peekable();
    let mut line = token.line;
    let mut value = String::new();
    let mut lines = Vec::new();
    while let Some(mut ch) = chars.next() {
        let origin = line;
        if ch == '\n' {
            line += 1;
        }
        if ch == '\\' && !raw {
            let Some(escape) = chars.next() else {
                break;
            };
            ch = match escape {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '0' => '\0',
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .unwrap_or('\u{fffd}')
                }
                'u' => {
                    let _ = chars.next();
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').filter(|c| *c != '_').collect();
                    u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .unwrap_or('\u{fffd}')
                }
                '\n' | '\r' => {
                    line += usize::from(escape == '\n');
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        line += usize::from(chars.next() == Some('\n'));
                    }
                    continue;
                }
                other => other,
            };
        }
        value.push(ch);
        lines.extend(std::iter::repeat_n(origin, ch.len_utf8()));
    }
    (value, lines)
}

pub(super) fn remote_lines(value: &str, lines: &[usize]) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    for (index, _) in value.match_indices("://") {
        let tail = &value[index + 3..];
        let authority = tail
            .split(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | '\\' | '<' | '>'))
            .next()
            .unwrap_or_default();
        if !authority.is_empty() && !local_authority(authority) {
            found.insert(lines[index]);
        }
    }
    let mut offset = 0;
    for part in
        value.split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']' | '%' | '@')))
    {
        if let Some((host, port)) = part.rsplit_once(':')
            && !host.is_empty()
            && !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
            && !local_authority(part)
        {
            found.insert(lines[offset]);
        }
        offset += part.len() + 1;
        // split delimiters may be multibyte; find the next character boundary.
        while offset < value.len() && !value.is_char_boundary(offset) {
            offset += 1;
        }
    }
    found
}

fn local_authority(authority: &str) -> bool {
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let (host, port) = if let Some(tail) = host_port.strip_prefix('[') {
        let Some((host, tail)) = tail.split_once(']') else {
            return false;
        };
        (
            host,
            tail.strip_prefix(':').or(if tail.is_empty() { Some("") } else { None }),
        )
    } else {
        let (host, port) = host_port.split_once(':').unwrap_or((host_port, ""));
        (host, Some(port))
    };
    matches!(host, "127.0.0.1" | "::1" | "localhost")
        && port.is_some_and(|port| port.is_empty() || port.parse::<u16>().is_ok())
}
