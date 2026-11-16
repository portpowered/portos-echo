//! T1: raw evidence joins; deliberately no sockets or processes.
use super::*;

fn world(negative: bool) -> World {
    let mut world = World::default();
    for event in [
        "session.delegation.created",
        "muse.submit",
        "muse.progress",
        "muse.terminal",
        "session.commentary.append",
        "reply.played",
        "reply.started",
    ] {
        world
            .trace
            .records
            .push(json!({"event":event,"elapsed_ms":0,"run_id":"run-1","delegation_id":"d-1","task_id":"t-1"}));
    }
    world.trace.records[3]["status"] = json!(if negative { "failed" } else { "completed" });
    if negative {
        world.trace.records[3]["retryable"] = json!(false);
    }
    world.completion = json!({"event":"bridge.run_completed","run_id":"run-1"});
    world.playback = json!({"event":"host.ack","pcm_bytes":9600,"nonzero_bytes":0});
    world.muse.calls = vec![
        json!({"argv":["send-user-msg","--session-id","00000000-0000-4000-8000-000000000001","--wait","1",
        "Execute the delegated request and report its result.\nContext: {\"delegation\":{\"id\":\"d-1\",\"target\":\"client\"}}\nTask ID: t-1\nWhen you are finished, call the echo.report.result command with task_id t-1, status completed or failed, and a short summary. Do not report success any other way."],"stdin":""}),
    ];
    if !negative {
        world
            .muse
            .reports
            .push(json!({"exit":0,"stderr":"","stdout":"{\"accepted\":true,\"task_id\":\"t-1\"}"}));
    }
    let mut sent = vec![DialogueSent::Text(
        json!({"type":"response.audio_transcript.done","transcript":if negative { DEADLINE } else { SUCCESS }})
            .to_string(),
    )];
    sent.extend((0..10).map(|sequence| {
        DialogueSent::Audio(echo_live::AudioFrame {
            sequence,
            timestamp: u32::from(sequence) * 960,
            payload: vec![0xf8, 0xff, 0xfe],
        })
    }));
    world.dialogue = vec![echo_live::fake::DialogueCall {
        call_index: 0,
        delegation_id: "d-1".into(),
        sent,
        rejected_commentary: 0,
        received_audio: 44,
        reply_completed: true,
        closed: true,
        failure: None,
    }];
    world
}

#[test]
fn m7_success_and_deadline_join_exact_raw_observations() {
    for negative in [false, true] {
        let measured = observe(&world(negative), negative).unwrap();
        assert_eq!(measured["task_id"], "t-1");
        assert_eq!(measured["delegation_id"], "d-1");
        assert_eq!(measured["reply_pcm_bytes"], 9600);
        assert_eq!(measured["completed_count"], usize::from(!negative));
        assert_eq!(measured["report_task_id"], if negative { "" } else { "t-1" });
    }
}

#[test]
fn m7_identity_and_duplicate_diagnostics_name_expected_and_observed() {
    let mut wrong = world(false);
    wrong.trace.records[2]["task_id"] = json!("another");
    assert_eq!(
        observe(&wrong, false).unwrap_err(),
        "m7 raw task ID: expected \"t-1\"; observed \"another\""
    );
    let mut wrong = world(false);
    wrong.trace.records.push(wrong.trace.records[3].clone());
    assert_eq!(
        observe(&wrong, false).unwrap_err(),
        "m7 muse.terminal: expected 1; observed 2"
    );
    let mut wrong = world(false);
    wrong.trace.records.push(wrong.trace.records[6].clone());
    assert_eq!(
        observe(&wrong, false).unwrap_err(),
        "m7 reply.started: expected 1; observed 2"
    );
}

#[test]
fn m7_wrong_missing_or_stale_identity_and_extra_outcomes_are_rejected() {
    for index in 0..16 {
        let mut wrong = world(false);
        match index {
            0 => {
                wrong.trace.records.remove(1);
            }
            1 => {
                wrong.trace.records[2]["task_id"] = json!("another");
            }
            2 => {
                wrong.trace.records[3]["delegation_id"] = json!("another");
            }
            3 => {
                wrong.completion["run_id"] = json!("stale");
            }
            4 => {
                wrong.trace.records.push(wrong.trace.records[3].clone());
            }
            5 => {
                wrong.trace.records.push(wrong.trace.records[5].clone());
            }
            6 => {
                wrong.muse.calls.clear();
            }
            7 => {
                wrong.muse.calls[0]["argv"][5] = json!("wrong task prompt");
            }
            8 => {
                wrong.muse.reports[0]["stdout"] = json!("{\"accepted\":true,\"task_id\":\"stale\"}");
            }
            9 => {
                wrong.muse.reports[0]["exit"] = json!(1);
            }
            10 => {
                wrong.dialogue[0].delegation_id = "stale".into();
            }
            11 => {
                wrong.dialogue[0].sent.pop();
            }
            12 => {
                wrong.playback["pcm_bytes"] = json!(0);
            }
            13 => {
                wrong.model_requests = 1;
            }
            14 => {
                wrong.tool_calls = 1;
            }
            _ => {
                wrong.trace.records.swap(2, 3);
            }
        }
        assert!(observe(&wrong, false).is_err(), "mutation {index}");
    }
    let mut wrong = world(true);
    wrong.muse.reports.push(json!({"accepted":true}));
    assert!(observe(&wrong, true).is_err());
}
