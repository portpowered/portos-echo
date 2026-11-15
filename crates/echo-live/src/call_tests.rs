//! T1 configuration and T2 provisional wire behavior; no sockets.
use echo_contracts::ids::{DelegationId, SessionId};
use echo_contracts::live::{DelegationTarget, LiveEvent};
use echo_contracts::secret::Secret;
use serde_json::json;

use crate::call::{CallError, LiveAuthMode, LiveCredential, SessionOptions, commentary, decode};
use crate::{Error, call_http};

fn options(mode: LiveAuthMode) -> SessionOptions {
    SessionOptions {
        model: match mode {
            LiveAuthMode::Chatgpt => "gpt-live-1-codex",
            LiveAuthMode::ApiKey => "gpt-live-1",
        }
        .into(),
        voice: "synthetic-voice".into(),
        instructions: "Reply with snowman ☃".into(),
        session_id: SessionId::new("synthetic-session").unwrap(),
        thread_id: "synthetic-thread".into(),
    }
}

#[test]
fn call_logic_endpoints_reject_unsafe_origins() {
    for base in [
        "bad",
        "http://localhost", // lint-rules: allow-literal inert URL validation input, never dialed
        "http://192.0.2.1", // lint-rules: allow-literal inert URL validation input, never dialed
        "ftp://127.0.0.1",
        "https://user:password@invalid.test", // lint-rules: allow-literal inert URL validation input, never dialed
        "https://invalid.test/path",          // lint-rules: allow-literal inert URL validation input, never dialed
        "https://invalid.test/?key=x",        // lint-rules: allow-literal inert URL validation input, never dialed
        "https://invalid.test/#x",            // lint-rules: allow-literal inert URL validation input, never dialed
    ] {
        assert_eq!(
            call_http::endpoint(base, LiveAuthMode::ApiKey),
            Err(CallError::Configuration)
        );
    }
    let base = "http://127.0.0.1:1234";
    assert_eq!(
        call_http::endpoint(base, LiveAuthMode::Chatgpt).unwrap().as_str(),
        "http://127.0.0.1:1234/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas"
    );
    assert_eq!(
        call_http::endpoint(base, LiveAuthMode::ApiKey).unwrap().as_str(),
        "http://127.0.0.1:1234/v1/live/sessions"
    );
    assert_eq!(
        call_http::endpoint("https://invalid.test", LiveAuthMode::ApiKey) // lint-rules: allow-literal inert URL parser assertion, never dialed
            .unwrap()
            .scheme(),
        "https"
    );
}

#[test]
fn call_logic_models_and_header_validation() {
    for mode in [LiveAuthMode::Chatgpt, LiveAuthMode::ApiKey] {
        let mut opts = options(mode);
        assert_eq!(call_http::validate_options(mode, &opts), Ok(()));
        opts.model = "wrong".into();
        assert_eq!(call_http::validate_options(mode, &opts), Err(CallError::Configuration));
        opts = options(mode);
        opts.voice.clear();
        assert_eq!(call_http::validate_options(mode, &opts), Err(CallError::Configuration));
        opts = options(mode);
        opts.thread_id.clear();
        assert_eq!(call_http::validate_options(mode, &opts), Err(CallError::Configuration));
        opts.thread_id = "injected\r\nheader".into();
        assert_eq!(call_http::validate_options(mode, &opts), Err(CallError::Configuration));
    }
}

#[test]
fn call_logic_headers_keep_modes_isolated_and_sensitive() {
    let chatgpt = LiveCredential::Chatgpt {
        token: Secret::new("synthetic-token".into()),
        account_id: "synthetic-account".into(),
    };
    let chat = call_http::headers(LiveAuthMode::Chatgpt, &chatgpt, &options(LiveAuthMode::Chatgpt)).unwrap();
    assert_eq!(chat.len(), 8);
    assert!(chat["authorization"].as_bytes() == b"Bearer synthetic-token");
    assert!(chat["authorization"].is_sensitive());
    assert!(chat["chatgpt-account-id"].as_bytes() == b"synthetic-account");
    assert_eq!(chat["openai-alpha"], "quicksilver=v2");
    assert_eq!(chat["originator"], "portos-echo");
    assert_eq!(chat["user-agent"], "portos-echo/0.1.0");
    assert_eq!(chat["session-id"], "synthetic-session");
    assert_eq!(chat["x-session-id"], "synthetic-session");
    assert_eq!(chat["thread-id"], "synthetic-thread");
}

#[test]
fn call_logic_api_headers_reject_mismatched_credentials() {
    let chatgpt = LiveCredential::Chatgpt {
        token: Secret::new("synthetic-token".into()),
        account_id: "synthetic-account".into(),
    };
    let api = LiveCredential::ApiKey {
        key: Secret::new("synthetic-key".into()),
    };
    let headers = call_http::headers(LiveAuthMode::ApiKey, &api, &options(LiveAuthMode::ApiKey)).unwrap();
    assert_eq!(headers.len(), 2);
    assert!(headers["authorization"].as_bytes() == b"Bearer synthetic-key");
    assert!(headers["authorization"].is_sensitive());
    assert_eq!(headers["user-agent"], "portos-echo/0.1.0");
    assert_eq!(
        call_http::headers(LiveAuthMode::ApiKey, &chatgpt, &options(LiveAuthMode::ApiKey)),
        Err(CallError::Configuration)
    );
    assert_eq!(
        call_http::headers(LiveAuthMode::Chatgpt, &api, &options(LiveAuthMode::Chatgpt)),
        Err(CallError::Configuration)
    );
}

#[test]
fn call_logic_bad_credentials_have_fixed_diagnostics() {
    for secret in ["", "synthetic\r\ncredential"] {
        let credential = LiveCredential::ApiKey {
            key: Secret::new(secret.into()),
        };
        let error = call_http::headers(LiveAuthMode::ApiKey, &credential, &options(LiveAuthMode::ApiKey)).unwrap_err();
        assert_eq!(error, CallError::Configuration);
        assert_eq!(error.to_string(), "invalid Live configuration");
    }
    let credential = LiveCredential::Chatgpt {
        token: Secret::new("synthetic".into()),
        account_id: String::new(),
    };
    assert_eq!(
        call_http::headers(LiveAuthMode::Chatgpt, &credential, &options(LiveAuthMode::Chatgpt)),
        Err(CallError::Configuration)
    );
    assert_eq!(CallError::Decode.to_string(), "invalid Live event");
    assert_eq!(
        CallError::Authorization.to_string(),
        "Live authorization requires relinking"
    );
}

#[test]
fn call_logic_commentary_bounds_apply_after_json_escaping() {
    let id = DelegationId::new("synthetic-delegation-1").unwrap();
    assert_eq!(commentary(id.clone(), "a".repeat(16_384)), Err(Error::Full.into()));
    assert_eq!(commentary(id.clone(), "\n".repeat(8192)), Err(Error::Full.into()));
    let overhead = commentary(id.clone(), String::new()).unwrap().len();
    assert_eq!(
        commentary(id.clone(), "a".repeat(16_384 - overhead)).unwrap().len(),
        16_384
    );
    assert_eq!(commentary(id, "a".repeat(16_385 - overhead)), Err(Error::Full.into()));
}

#[test]
fn call_wire_session_body_is_client_only() {
    for mode in [LiveAuthMode::Chatgpt, LiveAuthMode::ApiKey] {
        let opts = options(mode);
        assert_eq!(
            call_http::offer_body("synthetic-sdp", &opts),
            json!({
                "sdp": "synthetic-sdp", "session": {"model": opts.model, "voice": "synthetic-voice",
                    "instructions": "Reply with snowman ☃", "delegation": {"target": "client"}}
            })
        );
    }
}

#[test]
fn call_wire_fake_script_rejects_unmarked_or_malformed_documents() {
    for text in [
        "invalid json",
        r#"{"events":[]}"#,
        r#"{"synthetic":false,"events":[]}"#,
        r#"{"synthetic":true}"#,
        r#"{"synthetic":true,"events":{}}"#,
    ] {
        assert_eq!(crate::fake::FakeScript::from_json(text).err(), Some(CallError::Decode));
    }
    let script = crate::fake::FakeScript::from_json(r#"{"synthetic":true,"events":[]}"#).unwrap();
    assert_eq!(script.events, Vec::<String>::new());
    assert!(matches!(script.response, crate::fake::FakeResponse::Answer));
}

#[test]
fn call_wire_synthetic_script_preserves_exact_values() {
    let script: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/live/session.synthetic.json")).unwrap();
    assert_eq!(script["synthetic"], true);
    let events: Vec<_> = script["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| decode(&value.to_string()).unwrap())
        .collect();
    assert_eq!(events.len(), 4);
    assert_eq!(
        events[0],
        LiveEvent::SessionCreated {
            extra: [(
                "session".into(),
                json!({"id": "synthetic-session", "future_field": true})
            )]
            .into()
        }
    );
    let LiveEvent::DelegationCreated { delegation, extra } = &events[1] else {
        assert!(matches!(&events[1], LiveEvent::DelegationCreated { .. }));
        return;
    };
    assert_eq!(delegation.id, DelegationId::new("synthetic-delegation-1").unwrap());
    assert_eq!(delegation.target, DelegationTarget::Client);
    assert_eq!(delegation.extra, [("future_field".into(), json!(17))].into());
    assert_eq!(*extra, std::collections::BTreeMap::new());
    assert_eq!(
        events[2],
        LiveEvent::Other(json!({"type": "future.event", "payload": {"value": 17}}))
    );
    assert_eq!(
        events[3],
        LiveEvent::Error {
            extra: [("error".into(), json!({"code": "synthetic_error"}))].into()
        }
    );
    for (event, original) in events.iter().zip(script["events"].as_array().unwrap()) {
        assert_eq!(serde_json::to_value(event).unwrap(), *original);
    }
}

#[test]
fn call_wire_malformed_known_events_fail_and_responses_stays_observable() {
    for text in [
        "not json",
        r#"{"type":"session.delegation.created"}"#,
        r#"{"type":"session.delegation.created","delegation":{"id":"","target":"client"}}"#,
        r#"{"type":"session.commentary.append","delegation_id":"d"}"#,
    ] {
        assert_eq!(decode(text), Err(CallError::Decode));
    }
    let event =
        decode(r#"{"type":"session.delegation.created","delegation":{"id":"d","target":"responses"}}"#).unwrap();
    let LiveEvent::DelegationCreated { delegation, .. } = event else {
        assert!(matches!(&event, LiveEvent::DelegationCreated { .. }));
        return;
    };
    assert_eq!(delegation.target, DelegationTarget::Responses);
    let id = DelegationId::new("synthetic-delegation-1").unwrap();
    assert_eq!(
        decode(&commentary(id.clone(), "Reply ☃\n\"".into()).unwrap()).unwrap(),
        LiveEvent::CommentaryAppend {
            delegation_id: id,
            text: "Reply ☃\n\"".into(),
            extra: std::collections::BTreeMap::new()
        }
    );
}
