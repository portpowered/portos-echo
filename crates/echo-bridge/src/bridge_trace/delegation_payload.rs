//! Payload rules apply only to the authored delegation vocabulary.
use crate::Error;
use serde_json::{Map, Value, json};

const FIELDS: [&str; 4] = ["task", "request", "response", "answer"];
const CAP: usize = 65536;

pub(super) fn event(event: &str) -> bool {
    matches!(
        event,
        "delegation.created" | "session.delegation.created" | "delegation.tool" | "delegation.resolved"
    )
}

pub(super) fn prepare(fields: &mut Map<String, Value>, enabled: bool) -> Result<(), Error> {
    let mut payloads = Vec::new();
    for key in FIELDS {
        if let Some(mut value) = fields.remove(key)
            && enabled
        {
            redact(&mut value);
            let original_bytes = serde_json::to_vec(&value).map_err(|_| Error::Trace)?.len();
            if original_bytes > CAP {
                value = json!({"truncated":true,"original_bytes":original_bytes});
            }
            payloads.push((key.to_owned(), value));
        }
    }
    if payloads.len() > 2 {
        return Err(Error::Trace);
    }
    fields.extend(payloads);
    Ok(())
}

pub(super) fn metadata_fits(entry: &Map<String, Value>) -> Result<bool, Error> {
    let mut metadata = entry.clone();
    for key in FIELDS {
        metadata.remove(key);
    }
    Ok(serde_json::to_vec(&metadata).map_err(|_| Error::Trace)?.len() <= 4096)
}

fn sensitive(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        key.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "key"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "password"
            | "secret"
            | "clientsecret"
            | "credential"
            | "credentials"
            | "account"
            | "accountid"
            | "chatgptaccountid"
            | "installation"
            | "installationid"
            | "installid"
    )
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if sensitive(key) {
                    *value = json!("[redacted]");
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        Value::String(text) => *text = redact_text(text),
        _ => {}
    }
}

fn redact_text(text: &str) -> String {
    // Tool content often nests JSON inside a text content item. Preserve public
    // strings exactly unless redaction actually changes the embedded value.
    if let Ok(mut value) = serde_json::from_str::<Value>(text)
        && !value.is_string()
    {
        let before = value.clone();
        redact(&mut value);
        if value != before {
            return value.to_string();
        }
    }
    let mut bearer = false;
    text.split_inclusive(|c: char| !(c.is_ascii_alphanumeric() || "_.-/+=".contains(c)))
        .map(|part| {
            let token = part.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || "_.-/+=".contains(c)));
            let suffix = &part[token.len()..];
            let credential = bearer && !token.is_empty();
            if !token.is_empty() {
                bearer = token.eq_ignore_ascii_case("bearer");
            }
            // Periods delimit JWT segments, but can also surround a token in prose.
            let candidate = token.trim_matches('.');
            if jwt(candidate) {
                let prefix = &token[..token.len() - token.trim_start_matches('.').len()];
                let punctuation = &token[prefix.len() + candidate.len()..];
                format!("{prefix}[redacted]{punctuation}{suffix}")
            } else if credential || token.starts_with("sk-") {
                format!("[redacted]{suffix}")
            } else {
                part.to_owned()
            }
        })
        .collect()
}

fn jwt(token: &str) -> bool {
    let parts: Vec<_> = token.split('.').collect();
    parts.len() == 3 && parts[0].starts_with("eyJ") && parts.iter().all(|part| !part.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_trace_redacts_recursive_credentials_and_preserves_public_content() {
        let mut value = json!({"query":"synthetic-query-7", "url":"http://127.0.0.1/synthetic-source",
            "result":"synthetic-result-7", "nested":[{"Authorization":"synthetic-auth",
            "account_id":"synthetic-account", "installationId":"synthetic-install"}],
            "text":"Bearer synthetic-bearer eyJfixture.payload.signature",
            "content":"{\"token\":\"synthetic-token\",\"text\":\"synthetic-result-7\"}"});
        redact(&mut value);
        assert_eq!(
            value,
            json!({"query":"synthetic-query-7", "url":"http://127.0.0.1/synthetic-source",
            "result":"synthetic-result-7", "nested":[{"Authorization":"[redacted]",
            "account_id":"[redacted]", "installationId":"[redacted]"}],
            "text":"Bearer [redacted] [redacted]",
            "content":"{\"text\":\"synthetic-result-7\",\"token\":\"[redacted]\"}"})
        );
    }

    #[test]
    fn delegation_trace_writer_redacts_jwt_at_punctuation_boundaries() {
        use crate::bridge_trace::Trace;
        let root = tempfile::tempdir().unwrap();
        let secret = "eyJfixture.payload.signature";
        let input = [("", ""), ("", "."), ("", "..."), ("(", ")."), ("...", "., next")]
            .map(|(prefix, suffix)| format!("Token: {prefix}{secret}{suffix}"))
            .join("\n");
        let expected = json!({"text":input.replace(secret, "[redacted]")});
        for enabled in [true, false] {
            let path = root.path().join(enabled.to_string());
            let mut trace = Trace::open(&path, "api_key").unwrap();
            if !enabled {
                trace = trace.with_payloads(false);
            }
            for (event, field) in [
                ("delegation.created", "task"),
                ("delegation.tool", "request"),
                ("delegation.tool", "response"),
                ("delegation.resolved", "answer"),
            ] {
                trace
                    .record(event, Some("run-fixture"), json!({field:{"text":input}}))
                    .unwrap();
            }
            let text = std::fs::read_to_string(path).unwrap();
            assert!(!text.contains(secret));
            let mut rows = text.lines().map(|line| serde_json::from_str::<Value>(line).unwrap());
            for field in FIELDS {
                let row = rows.next().unwrap();
                if enabled {
                    assert_eq!(row[field], expected);
                } else {
                    assert!(row.get(field).is_none());
                    assert!(!text.contains("Token:"));
                }
            }
            assert!(rows.next().is_none());
        }
    }

    #[test]
    fn delegation_trace_payload_cap_is_exact_valid_json_and_multibyte_safe() {
        for size in [CAP - 1, CAP, CAP + 1] {
            let text = format!("{}{}", "é".repeat((size - 2) / 2), "x".repeat((size - 2) % 2));
            let payload = json!(text);
            assert_eq!(serde_json::to_vec(&payload).unwrap().len(), size);
            let mut fields = json!({"request":payload,"response":payload})
                .as_object()
                .unwrap()
                .clone();
            prepare(&mut fields, true).unwrap();
            for key in ["request", "response"] {
                let expected = if size > CAP {
                    json!({"truncated":true,"original_bytes":size})
                } else {
                    payload.clone()
                };
                assert_eq!(fields[key], expected);
                assert!(serde_json::to_vec(&fields[key]).unwrap().len() <= CAP);
            }
        }
    }

    #[test]
    fn delegation_trace_disabled_removes_every_payload_and_keeps_metadata() {
        let mut fields = json!({"task":"task", "request":"query", "response":"result", "answer":"answer",
            "outcome":"completed", "tool_counts":{"mcp":1}})
        .as_object()
        .unwrap()
        .clone();
        prepare(&mut fields, false).unwrap();
        assert_eq!(
            Value::Object(fields),
            json!({"outcome":"completed","tool_counts":{"mcp":1}})
        );
    }

    #[test]
    fn delegation_trace_writer_round_trips_capped_payloads_and_opt_out_on_clones() {
        use crate::bridge_trace::Trace;
        let root = tempfile::tempdir().unwrap();
        for enabled in [true, false] {
            let path = root.path().join(enabled.to_string());
            let mut trace = Trace::open(&path, "api_key").unwrap().with_payloads(enabled);
            let payload = json!({"text":"é".repeat(CAP), "authorization":"synthetic-auth"});
            let mut redacted = payload.clone();
            redact(&mut redacted);
            let size = serde_json::to_vec(&redacted).unwrap().len();
            trace
                .clone()
                .record(
                    "delegation.tool",
                    Some("run"),
                    json!({"request":payload,"response":payload,"kind":"mcp","status":"completed"}),
                )
                .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let row: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(row["event"], "delegation.tool");
            assert_eq!(row["run_id"], "run");
            assert_eq!(row["provider_auth_mode"], "api_key");
            assert_eq!(row["kind"], "mcp");
            assert_eq!(row["status"], "completed");
            for key in ["request", "response"] {
                if enabled {
                    assert_eq!(row[key], json!({"truncated":true,"original_bytes":size}));
                } else {
                    assert!(row.get(key).is_none());
                }
            }
            assert!(!String::from_utf8(bytes.clone()).unwrap().contains("synthetic-auth"));
            assert_eq!(
                trace.record("run.started", None, json!({"text":"x".repeat(4096)})),
                Err(Error::Trace)
            );
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn delegation_trace_cap_measures_redacted_bytes_and_rejects_excess_payload_fields() {
        let mut fields = json!({"request":{"token":"x".repeat(CAP)}})
            .as_object()
            .unwrap()
            .clone();
        prepare(&mut fields, true).unwrap();
        assert_eq!(fields["request"], json!({"token":"[redacted]"}));
        let mut fields = json!({"task":null,"request":null,"response":null})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(prepare(&mut fields, true), Err(Error::Trace));
        let mut fields = json!({"metadata":"x".repeat(4096)}).as_object().unwrap().clone();
        prepare(&mut fields, true).unwrap();
        assert!(!metadata_fits(&fields).unwrap());
    }
}
