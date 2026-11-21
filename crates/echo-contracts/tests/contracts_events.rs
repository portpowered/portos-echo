//! T2 exact synthetic task/Live messages; uncaptured Wyoming arms are synthetic.
use echo_contracts::endpoint::{
    AssistantState, CommandReply, Downlink, ECHO_PLUGIN, EndSession, EndSessionOutcome, EndedReason, SessionEnded,
    SessionOrigin, StartSession, StartSessionOutcome, Uplink, VERSION,
};
use echo_contracts::{ids::DelegationId, live::*, wyoming::*};
use serde_json::to_value;
use serde_json::{Value, json};

#[test]
fn provisional_live_tags_targets_and_extras() {
    for (target, expected_target) in [
        ("client", DelegationTarget::Client),
        ("responses", DelegationTarget::Responses),
    ] {
        let wire = json!({"type":"session.delegation.created", "delegation": {
            "id":"delegation-1", "target":target,"opaque":[1,2]}, "outer":true});
        let event: LiveEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            event,
            LiveEvent::DelegationCreated {
                delegation: Delegation {
                    id: DelegationId::new("delegation-1").unwrap(),
                    target: expected_target,
                    extra: [("opaque".into(), json!([1, 2]))].into(),
                },
                extra: [("outer".into(), json!(true))].into(),
            }
        );
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }
    for wire in [
        json!({"type":"session.commentary.append","delegation_id":"delegation-1","text":"result","extra":3}),
        json!({"type":"session.created","session":{"opaque":true}}),
        json!({"type":"error","error":{"message":"synthetic"}}),
    ] {
        let event: LiveEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(!matches!(event, LiveEvent::Other(_)));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }
}

#[test]
fn unknown_live_values_and_wyoming_tags_survive() {
    for wire in [
        json!({"type":"future","nested":[null,true]}),
        json!(42),
        json!(["future"]),
        Value::Null,
        json!({}),
        json!({"type":5}),
    ] {
        let event: LiveEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event, LiveEvent::Other(wire.clone()));
        assert_eq!(serde_json::to_value(event).unwrap(), wire);
    }
    for data in [Value::Null, json!([1, true]), json!({"opaque":"value"})] {
        let wire = json!({"type":"future","data":data});
        let event: WyomingEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event, WyomingEvent::Other("future".into(), data));
        assert_eq!(serde_json::to_value(event).unwrap(), wire);
    }
}

#[test]
fn known_malformed_messages_are_errors() {
    for wire in [
        json!({"type":"session.delegation.created"}),
        json!({"type":"session.delegation.created","delegation":{"id":"","target":"client"}}),
        json!({"type":"session.delegation.created","delegation":{"id":"d","target":"unknown"}}),
        json!({"type":"session.commentary.append","delegation_id":"d","text":7}),
        json!({"type":"session.commentary.append","text":"missing"}),
    ] {
        assert!(serde_json::from_value::<LiveEvent>(wire).is_err());
    }
    for kind in ["audio-start", "audio-chunk", "run-pipeline"] {
        for data in [Value::Null, json!({}), json!({"rate":"bad","width":2,"channels":1})] {
            let header: Header = serde_json::from_value(json!({"type":kind,"data":data})).unwrap();
            assert!(
                Event {
                    header,
                    data: None,
                    payload: vec![]
                }
                .typed()
                .is_err()
            );
        }
    }
}

#[test]
fn uncaptured_wyoming_tags_are_opaque_named_arms() {
    for kind in ["detection", "transcript", "voice-stopped", "error"] {
        let wire = json!({"type":kind,"data":{"synthetic":[1,"x"]}});
        let event: WyomingEvent = serde_json::from_value(wire.clone()).unwrap();
        let actual = match &event {
            WyomingEvent::Detection(_) => "detection",
            WyomingEvent::Transcript(_) => "transcript",
            WyomingEvent::VoiceStopped(_) => "voice-stopped",
            WyomingEvent::Error(_) => "error",
            _ => "unexpected",
        };
        assert_eq!(actual, kind);
        assert_eq!(serde_json::to_value(event).unwrap(), wire);
    }
}

#[test]
fn external_data_replaces_non_object_and_missing_inline() {
    for inline in [None, Some(json!(3)), Some(json!({"inline":true}))] {
        let header = Header {
            r#type: "future".into(),
            data: inline,
            data_length: 0,
            payload_length: 0,
        };
        let event = Event {
            header,
            data: Some(json!(["external"])),
            payload: vec![],
        };
        assert_eq!(
            event.typed().unwrap(),
            WyomingEvent::Other("future".into(), json!(["external"]))
        );
    }
}

#[test]
fn endpoint_start_command_preserves_route_and_text() {
    let text = "é".repeat(2_000);
    let command: Downlink = serde_json::from_value(json!({
        "header": {"namespace":"assistant-session","name":"start-session","version":VERSION,"messageId":"request-1"},
        "target": {"type":"ROUTE","value":"resolved-route"},
        "payload": {"initialText":text,"reason":"synthetic reason"}
    }))
    .unwrap();
    assert_eq!(command.header.namespace, "assistant-session");
    assert_eq!(command.header.name, "start-session");
    assert_eq!(command.header.version, "1.0");
    assert_eq!(command.header.message_id, "request-1");
    assert_eq!(command.target.value, "resolved-route");
    let payload: StartSession = serde_json::from_value(Value::Object(command.payload)).unwrap();
    assert_eq!(payload.initial_text.as_deref(), Some(text.as_str()));
    assert!(payload.listen);
    assert_eq!(payload.reason, "synthetic reason");
}

#[test]
fn endpoint_command_rejects_bad_wire_types() {
    for payload in [
        json!({"initialText":"missing reason"}),
        json!({"reason":"test","initialText":null}),
        json!({"reason":"test","initialText":"x".repeat(2_001)}),
        json!({"reason":"test","listen":"false"}),
    ] {
        assert!(serde_json::from_value::<StartSession>(payload).is_err());
    }
    let mic: StartSession = serde_json::from_value(json!({"reason":"mic","listen":false})).unwrap();
    assert_eq!(mic.initial_text, None);
    assert!(!mic.listen);
    for (kind, payload) in [("DEVICE", json!({})), ("ROUTE", json!([]))] {
        let command = json!({
            "header":{"namespace":"assistant-session","name":"end-session","version":VERSION,"messageId":"request-2"},
            "target":{"type":kind,"value":"resolved-route"},"payload":payload
        });
        assert!(serde_json::from_value::<Downlink>(command).is_err());
    }
}

#[test]
fn endpoint_session_replies_keep_request_ids_and_resolved_route() {
    let receipt = CommandReply {
        request_id: "request-1",
        outcome: StartSessionOutcome::admitted("session-1", AssistantState::Thinking),
    };
    assert_eq!(
        to_value(Uplink::new("start-session", "resolved-route", receipt)).unwrap(),
        json!({
            "header":{"namespace":ECHO_PLUGIN,"name":"start-session","version":"1.0"},
            "target":{"type":"ROUTE","value":"resolved-route"},
            "payload":{"requestId":"request-1","outcome":{"ok":true,"sessionId":"session-1","state":"thinking"}}
        })
    );
    let end: EndSession = serde_json::from_value(json!({"sessionId":"session-1"})).unwrap();
    assert_eq!(end.session_id, "session-1");
    let receipt = CommandReply {
        request_id: "request-2",
        outcome: EndSessionOutcome::closed(),
    };
    assert_eq!(
        to_value(Uplink::new("end-session", "resolved-route", receipt)).unwrap(),
        json!({
            "header":{"namespace":ECHO_PLUGIN,"name":"end-session","version":"1.0"},
            "target":{"type":"ROUTE","value":"resolved-route"},
            "payload":{"requestId":"request-2","outcome":{"ok":true,"state":"idle","ending":false}}
        })
    );
}

#[test]
fn endpoint_session_ended_is_unsolicited_and_preserves_identity() {
    let ended = SessionEnded {
        session_id: "session-1",
        origin: SessionOrigin::Wake,
        reason: EndedReason::Remote,
        ended_at: "2026-11-21T12:00:00Z",
    };
    assert_eq!(
        to_value(Uplink::new("session-ended", "resolved-route", ended)).unwrap(),
        json!({
            "header":{"namespace":ECHO_PLUGIN,"name":"session-ended","version":"1.0"},
            "target":{"type":"ROUTE","value":"resolved-route"},
            "payload":{"sessionId":"session-1","origin":"wake","reason":"remote","endedAt":"2026-11-21T12:00:00Z"}
        })
    );
}
