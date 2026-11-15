//! Private, dependency-free JSON reader for Cargo metadata format version 1.
use super::{Members, normalize, relative};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq)]
enum Value {
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    Scalar,
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
}

type Parsed<T> = Result<T, &'static str>;

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self.input.get(self.position).is_some_and(|b| b" \r\n\t".contains(b)) {
            self.position += 1;
        }
    }

    fn take(&mut self) -> Parsed<u8> {
        let byte = *self.input.get(self.position).ok_or("unexpected end of JSON")?;
        self.position += 1;
        Ok(byte)
    }

    fn require(&mut self, byte: u8) -> Parsed<()> {
        if self.take()? == byte {
            Ok(())
        } else {
            Err("unexpected JSON token")
        }
    }

    fn value(&mut self, depth: usize) -> Parsed<Value> {
        if depth > 128 {
            return Err("JSON nesting exceeds 128");
        }
        self.whitespace();
        match self.input.get(self.position) {
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => self.array(depth + 1),
            Some(b'{') => self.object(depth + 1),
            Some(b't' | b'f' | b'n') => self.literal(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err("expected JSON value"),
        }
    }

    fn string(&mut self) -> Parsed<String> {
        self.require(b'"')?;
        let mut bytes = Vec::new();
        loop {
            match self.take()? {
                b'"' => {
                    return String::from_utf8(bytes).map_err(|_| "invalid UTF-8 in JSON string");
                }
                b'\\' => self.escape(&mut bytes)?,
                0..=31 => return Err("unescaped control character in JSON string"),
                byte => bytes.push(byte),
            }
        }
    }

    fn hex(&mut self) -> Parsed<u32> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = char::from(self.take()?)
                .to_digit(16)
                .ok_or("invalid JSON unicode escape")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn escape(&mut self, bytes: &mut Vec<u8>) -> Parsed<()> {
        match self.take()? {
            byte @ (b'"' | b'\\' | b'/') => bytes.push(byte),
            b'b' => bytes.push(8),
            b'f' => bytes.push(12),
            b'n' => bytes.push(b'\n'),
            b'r' => bytes.push(b'\r'),
            b't' => bytes.push(b'\t'),
            b'u' => {
                let mut code = self.hex()?;
                if (0xd800..=0xdbff).contains(&code) {
                    self.require(b'\\')?;
                    self.require(b'u')?;
                    let low = self.hex()?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err("invalid JSON surrogate pair");
                    }
                    code = 0x10000 + (code - 0xd800) * 0x400 + low - 0xdc00;
                }
                let character = char::from_u32(code).ok_or("invalid JSON unicode scalar")?;
                bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
            }
            _ => return Err("invalid JSON escape"),
        }
        Ok(())
    }

    fn array(&mut self, depth: usize) -> Parsed<Value> {
        self.require(b'[')?;
        self.whitespace();
        let mut values = Vec::new();
        if self.input.get(self.position) == Some(&b']') {
            self.position += 1;
            return Ok(Value::Array(values));
        }
        loop {
            values.push(self.value(depth)?);
            self.whitespace();
            match self.take()? {
                b']' => return Ok(Value::Array(values)),
                b',' => {}
                _ => return Err("expected JSON array separator"),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Parsed<Value> {
        self.require(b'{')?;
        self.whitespace();
        let mut values = BTreeMap::new();
        if self.input.get(self.position) == Some(&b'}') {
            self.position += 1;
            return Ok(Value::Object(values));
        }
        loop {
            self.whitespace();
            let key = self.string()?;
            self.whitespace();
            self.require(b':')?;
            if values.insert(key, self.value(depth)?).is_some() {
                return Err("duplicate JSON key");
            }
            self.whitespace();
            match self.take()? {
                b'}' => return Ok(Value::Object(values)),
                b',' => {}
                _ => return Err("expected JSON object separator"),
            }
        }
    }

    fn literal(&mut self) -> Parsed<Value> {
        let word: &[u8] = match self.input.get(self.position) {
            Some(b't') => b"true",
            Some(b'f') => b"false",
            _ => b"null",
        };
        for &byte in word {
            self.require(byte)?;
        }
        Ok(Value::Scalar)
    }

    fn digits(&mut self) -> Parsed<()> {
        let start = self.position;
        while self.input.get(self.position).is_some_and(u8::is_ascii_digit) {
            self.position += 1;
        }
        if self.position == start {
            Err("expected JSON number digit")
        } else {
            Ok(())
        }
    }

    fn number(&mut self) -> Parsed<Value> {
        if self.input.get(self.position) == Some(&b'-') {
            self.position += 1;
        }
        if self.input.get(self.position) == Some(&b'0') {
            self.position += 1;
        } else {
            self.digits()?;
        }
        if self.input.get(self.position) == Some(&b'.') {
            self.position += 1;
            self.digits()?;
        }
        if matches!(self.input.get(self.position), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.input.get(self.position), Some(b'+' | b'-')) {
                self.position += 1;
            }
            self.digits()?;
        }
        Ok(Value::Scalar)
    }
}

fn parse(text: &str) -> Parsed<Value> {
    let mut parser = Parser {
        input: text.as_bytes(),
        position: 0,
    };
    let value = parser.value(0)?;
    parser.whitespace();
    if parser.position != parser.input.len() {
        return Err("trailing JSON data");
    }
    Ok(value)
}

fn field<'a>(value: &'a Value, key: &str) -> Parsed<&'a Value> {
    match value {
        Value::Object(fields) => fields.get(key).ok_or("missing required metadata field"),
        _ => Err("expected metadata object"),
    }
}

fn string(value: &Value) -> Parsed<&str> {
    match value {
        Value::String(text) if !text.is_empty() => Ok(text),
        _ => Err("expected nonempty metadata string"),
    }
}

fn array(value: &Value) -> Parsed<&[Value]> {
    match value {
        Value::Array(values) => Ok(values),
        _ => Err("expected metadata array"),
    }
}

fn project(value: &Value) -> Result<Members, String> {
    let root = normalize(string(field(value, "workspace_root")?)?)?;
    let ids = array(field(value, "workspace_members")?)?
        .iter()
        .map(string)
        .collect::<Parsed<BTreeSet<_>>>()?;
    let mut found = BTreeSet::new();
    let mut result = Members::new();
    for package in array(field(value, "packages")?)? {
        let id = string(field(package, "id")?)?;
        if !ids.contains(id) {
            continue;
        }
        if !found.insert(id) {
            return Err("duplicate workspace member id".into());
        }
        let name = string(field(package, "name")?)?;
        let manifest = normalize(string(field(package, "manifest_path")?)?)?;
        let local = manifest
            .strip_prefix(&format!("{root}/"))
            .ok_or("workspace member manifest is outside repository")?;
        let directory = local
            .strip_suffix("/Cargo.toml")
            .ok_or("expected member Cargo.toml path")?;
        let directory = relative(directory)?;
        if result.insert(name.to_string(), directory).is_some() {
            return Err("duplicate workspace member name".into());
        }
    }
    if found != ids {
        return Err("workspace member id has no package record".into());
    }
    Ok(result)
}

pub(super) fn members(text: &str) -> Result<Members, String> {
    parse(text)
        .map_err(String::from)
        .and_then(|value| project(&value))
        .map_err(|e| format!("cargo metadata: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const METADATA: &str = r#"{
      "workspace_root":"C:\\repo", "workspace_members":["member"],
      "packages":[{"id":"dependency","name":false},
        {"id":"member","name":"xtask","manifest_path":"C:\\repo\\.\\xtask\\Cargo.toml"}],
      "unrelated":[null,true,false,123,-1.2e+3,{"nested":"workspace_members \\"},"\ud83d\ude00"]
    }"#;

    #[test]
    fn selects_members_by_id_and_decodes_windows_paths() {
        assert_eq!(members(METADATA), Ok(Members::from([("xtask".into(), "xtask".into())])));
        assert_eq!(
            parse(r#""\"\\\/\b\f\n\r\t\u0061\ud83d\ude00""#),
            Ok(Value::String("\"\\/\u{8}\u{c}\n\r\ta😀".into()))
        );
    }

    #[test]
    fn rejects_malformed_json_including_unrelated_values() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "{\"a\":1,\"a\":2}",
            "01",
            "1.",
            "1e",
            "-",
            "true false",
            "nul",
            r#""\q""#,
            r#""\ud800""#,
            r#""\udc00""#,
            r#""\ud800\u0000""#,
            "\"raw\nline\"",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse(&format!("{}0{}", "[".repeat(130), "]".repeat(130))).is_err());
        assert!(members(&METADATA.replace("null", "01")).is_err());
    }

    #[test]
    fn required_metadata_fields_fail_closed() {
        for (before, after) in [
            ("\"workspace_root\"", "\"wrong_root\""),
            (
                "\"workspace_members\":[\"member\"]",
                "\"workspace_members\":[\"missing\"]",
            ),
            ("\"name\":\"xtask\"", "\"name\":false"),
            ("\"manifest_path\"", "\"wrong_path\""),
            ("repo\\\\.\\\\xtask", "elsewhere\\\\xtask"),
            ("Cargo.toml", "Other.toml"),
        ] {
            assert!(members(&METADATA.replace(before, after)).is_err(), "{before}");
        }
        assert_eq!(
            members("{}"),
            Err("cargo metadata: missing required metadata field".into())
        );
        assert_eq!(
            super::super::outcome(members("{").map(|_| vec![])),
            (
                1,
                vec!["lint-shape: input: cargo metadata: unexpected end of JSON".into()]
            )
        );
    }
}
