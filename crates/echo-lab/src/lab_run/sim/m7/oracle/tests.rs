//! T1: raw evidence joins; deliberately no sockets or processes.
use super::*;

fn world(negative: bool) -> World {
    let mut world = World::default();
    for event in [
        "delegation.created",
        "item/tool/call",
        "muse.submit",
        "muse.progress",
        "muse.terminal",
        "delegation.tool",
        "reply.played",
    ] {
        world
            .trace
            .records
            .push(json!({"event":event,"elapsed_ms":0,"run_id":"run-1",
            "delegation_id":"d-1","task_id":"t-1","name":"ask_muse","kind":"agent",
            "thread_id":"thread","turn_id":"turn","call_id":"tool-local","request_id":7}));
    }
    let status = json!(if negative { "failed" } else { "completed" });
    world.trace.records[4]["status"] = status.clone();
    world.trace.records[5]["status"] = status;
    world.trace.records[6]["outcome"] = json!("played");
    if negative {
        world.trace.records[4]["retryable"] = json!(false);
    }
    world.completion = json!({"event":"bridge.run_completed","run_id":"run-1"});
    world.playback = json!({"event":"host.ack","pcm_bytes":9600,"nonzero_bytes":9000});
    world.audibility = json!({"pcm_bytes":9600,"nonzero_samples":4800,"audible_samples":4700,"host":world.playback});
    world.ui_states = [
        "ready",
        "listening",
        "connected",
        "thinking",
        "speaking",
        "connected",
        "ready",
    ]
    .map(|state| json!({"state":state}))
    .into();
    world.muse.calls = vec![
        json!({"argv":["send-user-msg","--session-id","00000000-0000-4000-8000-000000000001","--wait","1",
        "Run the test job\nContext: null\nTask ID: t-1\nWhen you are finished, call the echo.report.result command with task_id t-1, status completed or failed, and a short summary. Do not report success any other way."],"stdin":""}),
    ];
    if !negative {
        world
            .muse
            .reports
            .push(json!({"exit":0,"stderr":"","stdout":"{\"accepted\":true,\"task_id\":\"t-1\"}"}));
    }
    world.muse.admission = json!({"task_id":"t-1","session_id":"00000000-0000-4000-8000-000000000001",
        "deadline_unix_seconds":109,"task":{"deadline_seconds":9,"delegation_id":"d-1"}});
    let answer = if negative { DEADLINE } else { SUCCESS };
    world.dialogue = json!({"delegations":["d-1"],"appends":["d-1"],"sidebands":1,"external":0,
        "tool_output":answer,
        "models":[{"outputs":0,"tools":["extend_session","end_session","ask_muse"]},
                  {"outputs":1,"tools":["extend_session","end_session","ask_muse"]}],
        "transcripts":[{"role":"user","text":"Computer, ask Muse to run the test job"},
                       {"role":"assistant","text":answer}]});
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
    wrong.trace.records.push(wrong.trace.records[4].clone());
    assert_eq!(
        observe(&wrong, false).unwrap_err(),
        "m7 muse.terminal: expected 1; observed 2"
    );
    let mut wrong = world(false);
    wrong.trace.records.push(wrong.trace.records[6].clone());
    assert_eq!(
        observe(&wrong, false).unwrap_err(),
        "m7 reply.played: expected 1; observed 2"
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
                wrong.trace.records.push(wrong.trace.records[4].clone());
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
                wrong.dialogue["delegations"] = json!(["stale"]);
            }
            11 => {
                wrong.dialogue["tool_output"] = json!("unexpected");
            }
            12 => {
                wrong.playback["pcm_bytes"] = json!(0);
            }
            13 => {
                wrong.dialogue["models"] = json!([]);
            }
            14 => {
                wrong.tool_calls = 1;
            }
            _ => {
                wrong.trace.records.swap(3, 4);
            }
        }
        assert!(observe(&wrong, false).is_err(), "mutation {index}");
    }
    let mut wrong = world(true);
    wrong.muse.reports.push(json!({"accepted":true}));
    assert!(observe(&wrong, true).is_err());
}

#[test]
fn m7_rejects_missing_silent_short_and_misordered_playback() {
    for field in ["pcm_bytes", "nonzero_samples", "audible_samples", "host"] {
        let mut wrong = world(false);
        wrong.audibility.as_object_mut().unwrap().remove(field);
        assert!(observe(&wrong, false).is_err(), "missing {field}");
    }
    for (field, value) in [
        ("pcm_bytes", 9598),
        ("pcm_bytes", 9602),
        ("nonzero_samples", 2399),
        ("audible_samples", 2399),
    ] {
        let mut wrong = world(false);
        wrong.audibility[field] = json!(value);
        assert!(observe(&wrong, false).is_err(), "{field}={value}");
    }
    let mut wrong = world(false);
    wrong.playback["nonzero_bytes"] = json!(0);
    wrong.audibility["host"] = wrong.playback.clone();
    assert!(observe(&wrong, false).is_err());
    for index in 0..5 {
        let mut wrong = world(false);
        wrong.ui_states.remove(index);
        assert!(observe(&wrong, false).is_err(), "missing state {index}");
    }
    let mut wrong = world(false);
    wrong.ui_states.swap(2, 3);
    assert!(observe(&wrong, false).is_err());
}

#[test]
fn m7_audibility_accepts_exact_thresholds() {
    let mut actual = world(false);
    actual.audibility["nonzero_samples"] = json!(2400);
    actual.audibility["audible_samples"] = json!(2400);
    assert!(observe(&actual, false).is_ok());
}
