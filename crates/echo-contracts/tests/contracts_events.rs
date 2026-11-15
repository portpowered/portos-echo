//! T2 exact synthetic task/Live messages; uncaptured Wyoming arms are synthetic.
use echo_contracts::{live::*, wyoming::*};
use serde_json::{Value, json};

#[test]
fn provisional_live_tags_targets_and_extras() {
    for target in ["client", "responses"] {
        let wire = json!({"type":"session.delegation.created", "delegation": {
            "id":"delegation-1", "target":target,"opaque":[1,2]}, "outer":true});
        let event: LiveEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(&event, LiveEvent::DelegationCreated { delegation, extra }
            if delegation.id.to_string() == "delegation-1" && extra["outer"] == true));
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
