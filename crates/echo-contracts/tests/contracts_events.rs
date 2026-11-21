//! T2 exact synthetic task/Live messages; uncaptured Wyoming arms are synthetic.
use echo_contracts::endpoint::{
    AdjustVolume, AssistantReport, AssistantState, ECHO_PLUGIN_ID, EndSession, EndSessionReply, EndedReason, Frame,
    InstallRequest, REPORT_NAMESPACE, Reply, Report, SessionEnded, SessionOrigin, SetOutputMute, SetVolume,
    SpeakerReport, StartSession, StartSessionReply, Uplink, VERSION,
};
use echo_contracts::{ids::DelegationId, live::*, wyoming::*};
use serde_json::to_value;
use serde_json::{Value, json};

#[test]
fn endpoint_speaker_payloads_take_numbers_and_boolean_output_mute() {
    let set: SetVolume = serde_json::from_value(json!({"volume":40.5})).unwrap();
    let adjust: AdjustVolume = serde_json::from_value(json!({"volumeDelta":-20})).unwrap();
    let mute: SetOutputMute = serde_json::from_value(json!({"mute":true})).unwrap();
    assert_eq!((set.volume, adjust.volume_delta, mute.mute), (40.5, -20.0, true));
    assert!(serde_json::from_value::<AdjustVolume>(json!({"delta":-20})).is_err());
    assert!(serde_json::from_value::<AdjustVolume>(json!({"volumeDelta":"20"})).is_err());
    assert!(serde_json::from_value::<SetOutputMute>(json!({"muted":true})).is_err());
}

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
fn endpoint_start_command_frame_names_interface_device_and_id() {
    let text = "é".repeat(2_000);
    let frame: Frame = serde_json::from_value(json!({
        "header": {"namespace":"port1/systems/zero/capability-interfaces/assistant-session",
            "name":"start-session","version":VERSION,"id":"request-1"},
        "body": {"initialText":text,"reason":"synthetic reason"},
        "context": {"deviceId":"synthetic-device","sentAt":"2026-11-21T12:00:00Z"}
    }))
    .unwrap();
    assert_eq!(frame.header.interface(), Some("assistant-session"));
    assert_eq!(frame.header.name, "start-session");
    assert_eq!(frame.header.version, "1.0");
    assert_eq!(frame.header.id, "request-1");
    assert_eq!(frame.context.device_id, "synthetic-device");
    let payload: StartSession = serde_json::from_value(frame.body).unwrap();
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
    let missing_context = json!({
        "header":{"namespace":"assistant-session","name":"end-session","version":VERSION,"id":"request-2"},
        "body":{}
    });
    assert!(serde_json::from_value::<Frame>(missing_context).is_err());
    let legacy: Frame = serde_json::from_value(json!({
        "header":{"namespace":"assistant-session","name":"end-session","version":VERSION,"id":"request-2"},
        "context":{"deviceId":"synthetic-device"}
    }))
    .unwrap();
    assert_eq!(legacy.header.interface(), None);
}

#[test]
fn endpoint_session_replies_are_nested_reports_with_one_request_id() {
    let start = Report::reply(
        "request-1",
        Reply::StartSession(StartSessionReply::admitted("session-1", AssistantState::Thinking)),
    );
    assert_eq!(
        to_value(Uplink::report("synthetic-device", start)).unwrap(),
        json!({
            "header":{"namespace":REPORT_NAMESPACE,"name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":"synthetic-device"},
            "payload":{"requestId":"request-1","startSession":{"sessionId":"session-1","state":"THINKING"}}
        })
    );
    let end: EndSession = serde_json::from_value(json!({"sessionId":"session-1"})).unwrap();
    assert_eq!(end.session_id, "session-1");
    for (muted, state) in [(false, "IDLE"), (true, "MUTED")] {
        let reply = Report::reply("request-2", Reply::EndSession(EndSessionReply::closed(muted)));
        assert_eq!(
            to_value(reply).unwrap(),
            json!({"requestId":"request-2","endSession":{"state":state}})
        );
    }
    assert_eq!(
        to_value(Report::reply("request-3", Reply::refused("NOT_FOUND"))).unwrap(),
        json!({"requestId":"request-3","error":{"code":"NOT_FOUND"}})
    );
    assert_eq!(
        to_value(Report::reply(
            "request-4",
            Reply::busy("session-1", SessionOrigin::Wake)
        ))
        .unwrap(),
        json!({"requestId":"request-4","error":{"code":"BUSY","sessionId":"session-1","origin":"WAKE"}})
    );
}

#[test]
fn endpoint_state_report_carries_session_only_while_active() {
    let report = Report {
        sampled_at_ms: Some(1_800_000_000_125),
        speaker: Some(SpeakerReport {
            volume: 40,
            muted: false,
        }),
        assistant: Some(AssistantReport::new(
            AssistantState::Listening,
            Some(("session-1", SessionOrigin::Remote)),
        )),
        ..Report::default()
    };
    assert_eq!(
        to_value(report).unwrap(),
        json!({
            "sampledAtMs":1_800_000_000_125_i64,
            "speaker":{"volume":40,"muted":false},
            "assistant":{"state":"LISTENING","sessionId":"session-1","origin":"REMOTE"}
        })
    );
    let idle = AssistantReport::new(AssistantState::Idle, Some(("session-1", SessionOrigin::Wake)));
    assert_eq!(to_value(idle).unwrap(), json!({"state":"IDLE"}));
}

#[test]
fn endpoint_snapshot_format_is_optional_and_the_reply_is_inline() {
    use echo_contracts::endpoint::{GetSnapshot, SnapshotFormat, SnapshotReply};

    let request: GetSnapshot = serde_json::from_value(json!({"responseFormat":"BASE64"})).unwrap();
    assert!(matches!(request.response_format, Some(SnapshotFormat::Base64)));
    let request: GetSnapshot = serde_json::from_value(json!({"responseFormat":"URL"})).unwrap();
    assert!(matches!(request.response_format, Some(SnapshotFormat::Url)));
    let request: GetSnapshot = serde_json::from_value(json!({})).unwrap();
    assert!(request.response_format.is_none());
    assert!(serde_json::from_value::<GetSnapshot>(json!({"responseFormat":"base64"})).is_err());
    let mut reply = Report::reply(
        "snapshot-1",
        Reply::Snapshot(SnapshotReply {
            width: 2,
            height: 1,
            data: "AA==".into(),
        }),
    );
    reply.sampled_at_ms = Some(1_800_000_000_125);
    assert_eq!(
        to_value(reply).unwrap(),
        json!({
            "sampledAtMs":1_800_000_000_125_i64,"requestId":"snapshot-1",
            "snapshot":{"width":2,"height":1,"data":"AA=="}
        })
    );
}

#[test]
fn endpoint_session_ended_is_unsolicited_and_preserves_identity() {
    let ended = Report {
        session_ended: Some(SessionEnded {
            session_id: "session-1".into(),
            origin: SessionOrigin::Wake,
            reason: EndedReason::Remote,
            ended_at: "2026-11-21T12:00:00Z".into(),
        }),
        ..Report::default()
    };
    assert_eq!(
        to_value(Uplink::report("synthetic-device", ended)).unwrap(),
        json!({
            "header":{"namespace":REPORT_NAMESPACE,"name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":"synthetic-device"},
            "payload":{"sessionEnded":{"sessionId":"session-1","origin":"WAKE","reason":"REMOTE","endedAt":"2026-11-21T12:00:00Z"}}
        })
    );
}

#[test]
fn endpoint_pairing_uses_the_published_plugin_id() {
    let install = to_value(InstallRequest::new("synthetic-device", "Hall Echo", None)).unwrap();
    assert_eq!(install["pluginId"], ECHO_PLUGIN_ID);
    assert_ne!(ECHO_PLUGIN_ID, REPORT_NAMESPACE);
    assert_eq!(install["principalId"], "~self");
}
