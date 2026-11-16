use crate::ScreenServerError;
use axum::http::Uri;
use echo_contracts::secret::Secret;
use std::fmt::Write;

/// Build a secret page URL with nested same-origin events/actions endpoints.
/// A base path is retained; endpoint paths are always rooted at the origin.
///
/// # Errors
/// Returns fixed diagnostics for a non-HTTP(S) base, userinfo, query, fragment,
/// invalid authority or empty token. No input is included in diagnostics.
pub fn page_url(base: &str, pairing_token: &Secret<String>) -> Result<Secret<String>, ScreenServerError> {
    let uri: Uri = base.parse().map_err(|_| ScreenServerError::InvalidPageUrl)?;
    validate_base(base, &uri)?;
    if pairing_token.expose().is_empty() {
        return Err(ScreenServerError::InvalidToken);
    }
    let token = encode(pairing_token.expose());
    let events = encode(&format!("/events?token={token}"));
    let actions = encode(&format!("/actions?token={token}"));
    Ok(Secret::new(format!(
        "{}/?events={events}&actions={actions}",
        base.trim_end_matches('/')
    )))
}

fn validate_base(base: &str, uri: &Uri) -> Result<(), ScreenServerError> {
    let authority = uri.authority().ok_or(ScreenServerError::InvalidPageUrl)?;
    let host = authority.host();
    let valid_host = if host.starts_with('[') && host.ends_with(']') {
        host[1..host.len() - 1].parse::<std::net::Ipv6Addr>().is_ok()
    } else {
        valid_domain(host)
    };
    let port = authority.as_str().strip_prefix(host).unwrap_or_default();
    let valid_port = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok());
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || !valid_host
        || !valid_port
        || authority.as_str().contains('@')
        || uri.query().is_some()
        || base.contains(['#', '\\'])
        || base.bytes().any(|b| b.is_ascii_control() || b == b' ')
    {
        return Err(ScreenServerError::InvalidPageUrl);
    }
    Ok(())
}

fn valid_domain(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    let last = host.rsplit('.').next().unwrap_or_default();
    // WHATWG treats numeric (including hex) final labels as IPv4 addresses.
    // Accept canonical IPv4 only, avoiding URI hosts the browser would reject.
    let numeric = last.bytes().all(|b| b.is_ascii_digit())
        || last
            .to_ascii_lowercase()
            .strip_prefix("0x")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()));
    !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        && (!numeric || host.parse::<std::net::Ipv4Addr>().is_ok())
}

fn encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            // Writing to String cannot fail.
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(value: &str) -> String {
        let mut bytes = Vec::new();
        let mut input = value.bytes();
        while let Some(byte) = input.next() {
            if byte == b'%' {
                let hex = [input.next().unwrap(), input.next().unwrap()];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap());
            } else {
                bytes.push(byte);
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    // Network-free oracle for transport.ts: root-relative endpoints resolve at
    // the page's HTTP(S) origin, with no userinfo; query decoding recovers token.
    fn parser_accepts(page: &str, token: &str) {
        let page: Uri = page.parse().unwrap();
        assert!(matches!(page.scheme_str(), Some("http" | "https")));
        let fields: Vec<_> = page.query().unwrap().split('&').collect();
        assert_eq!(fields.len(), 2);
        for (field, key) in fields.iter().zip(["events", "actions"]) {
            let (name, encoded) = field.split_once('=').unwrap();
            assert_eq!(name, key);
            let endpoint = decode(encoded);
            assert_eq!(endpoint, format!("/{key}?token={}", encode(token)));
            let resolved: Uri = format!(
                "{}://{}{}", // lint-rules: allow-literal T1 URI resolution only; no sockets
                page.scheme_str().unwrap(),
                page.authority().unwrap(),
                endpoint
            )
            .parse()
            .unwrap();
            assert_eq!(resolved.scheme(), page.scheme());
            assert_eq!(resolved.authority(), page.authority());
            assert!(!resolved.authority().unwrap().as_str().contains('@'));
            assert_eq!(decode(resolved.query().unwrap().strip_prefix("token=").unwrap()), token);
        }
    }

    #[test]
    fn page_url_exact_nested_encoding_and_parser_rules() {
        let token = Secret::new("synthetic-only".to_owned());
        let page = page_url("http://localhost:4173", &token).unwrap(); // lint-rules: allow-literal T1 URI validation only; no sockets
        assert_eq!(
            page.expose(),
            "http://localhost:4173/?events=%2Fevents%3Ftoken%3Dsynthetic-only&actions=%2Factions%3Ftoken%3Dsynthetic-only" // lint-rules: allow-literal T1 URI validation only; no sockets
        );
        parser_accepts(page.expose(), token.expose());
        let token = Secret::new("fake +&?=/%#é".to_owned());
        let page = page_url("https://example.invalid/screen/", &token).unwrap(); // lint-rules: allow-literal T1 URI validation only; no sockets
        assert_eq!(
            page.expose(),
            "https://example.invalid/screen/?events=%2Fevents%3Ftoken%3Dfake%2520%252B%2526%253F%253D%252F%2525%2523%25C3%25A9&actions=%2Factions%3Ftoken%3Dfake%2520%252B%2526%253F%253D%252F%2525%2523%25C3%25A9" // lint-rules: allow-literal T1 URI validation only; no sockets
        );
        parser_accepts(page.expose(), token.expose());
        for base in [
            "http://localhost/",               // lint-rules: allow-literal T1 URI validation only; no sockets
            "https://example.invalid/path///", // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://[::1]:4173",               // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://127.0.0.1:4173",           // lint-rules: allow-literal T1 URI validation only; no sockets
        ] {
            parser_accepts(page_url(base, &token).unwrap().expose(), token.expose());
        }
    }

    #[test]
    fn page_url_fixed_diagnostics() {
        let token = Secret::new("synthetic-only".to_owned());
        for base in [
            "",
            "/",
            "ftp://example.invalid", // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://",               // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://user:fake@localhost", // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://localhost/?token=fake", // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://localhost/#fake", // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://local\\host",    // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://localhost:99999", // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://localhost:bad",  // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://localhost:+80",  // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://999.999",        // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://0xFFFFFFFFF",    // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://local%40host",   // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://[bad]",          // lint-rules: allow-literal T1 URI validation only; no sockets
            "http://localhost/\n",   // lint-rules: allow-literal T1 URI validation only; no sockets
        ] {
            let err = page_url(base, &token).err().unwrap();
            assert_eq!(err.to_string(), "screen page URL is invalid");
            assert_eq!(format!("{err:?}"), "InvalidPageUrl");
        }
        let err = page_url("http://localhost", &Secret::new(String::new())).err().unwrap(); // lint-rules: allow-literal T1 URI validation only; no sockets
        assert_eq!(err.to_string(), "screen pairing token must not be empty");
        std::io::Write::write_all(&mut std::io::stdout(),
            b"page_url: exact=true; reserved=true; same-origin=true; invalid-base=screen page URL is invalid; empty-token=screen pairing token must not be empty\n").unwrap();
    }
}
