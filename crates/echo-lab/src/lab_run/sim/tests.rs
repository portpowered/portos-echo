use super::{observers, selectors, trace::Trace};

#[test]
fn only_supported_press_controls_are_mapped_and_fixture_reads_are_bounded() {
    for action in ["wake", "action", "pair", "stop", "mute"] {
        for value in [json!({"press":action}), json!({"press":{"key":action}})] {
            assert!(matches!(super::parse(&value).unwrap(), super::Step::Press(actual) if actual == action));
        }
    }
    assert!(super::parse(&json!({"press":"unknown"})).is_err());
    let path = std::env::temp_dir().join(format!("lab-bounded-{}", std::process::id()));
    std::fs::write(&path, b"abcd").unwrap();
    assert_eq!(observers::bytes(&path, 4, "fixture"), Ok(b"abcd".to_vec()));
    assert_eq!(
        observers::bytes(&path, 3, "fixture"),
        Err("fixture: observation exceeds size limit".into())
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        observers::bytes(&path, 3, "fixture"),
        Err("fixture: missing observation file".into())
    );
}
use serde_json::{Value, json};

fn records() -> Vec<Value> {
    vec![
        json!({"event":"wyoming.run_started","elapsed_ms":0,"run_id":"one"}),
        json!({"event":"irrelevant","elapsed_ms":1,"run_id":"one"}),
        json!({"event":"wyoming.mic_frames","count":10,"elapsed_ms":2,"run_id":"one"}),
        json!({"event":"wyoming.run_ended","elapsed_ms":3,"run_id":"one"}),
    ]
}

fn w1() -> Vec<selectors::Selector> {
    selectors::parse(&[
        json!("wyoming.run_started"),
        json!("wyoming.mic_frames {count_gte: 10}"),
        json!("wyoming.run_ended"),
    ])
    .unwrap()
}

#[test]
fn trace_sequence_rejects_missing_reordered_duplicated_and_mixed_runs() {
    assert_eq!(selectors::check(&records(), &w1(), None), Ok(()));
    let mut reordered = records();
    reordered.swap(0, 2);
    assert!(selectors::check(&reordered, &w1(), None).unwrap_err().contains("order"));
    let mut missing = records();
    missing.remove(2);
    assert!(
        selectors::check(&missing, &w1(), None)
            .unwrap_err()
            .contains("observed 0")
    );
    let mut duplicated = records();
    duplicated.push(records()[2].clone());
    assert!(
        selectors::check(&duplicated, &w1(), None)
            .unwrap_err()
            .contains("observed 2")
    );
    let mut mixed = records();
    mixed[2]["run_id"] = json!("other");
    assert!(
        selectors::check(&mixed, &w1(), None)
            .unwrap_err()
            .contains("observed 0")
    );
}

#[test]
fn frame_threshold_and_exact_call_count_are_distinct() {
    for (count, passes) in [(9, false), (10, true), (11, true)] {
        let mut trace = records();
        trace[2]["count"] = json!(count);
        assert_eq!(selectors::check(&trace, &w1(), Some("one")).is_ok(), passes);
    }
    let selector = selectors::parse(&[json!("tools/call {name: test.echo, count: 1}")]).unwrap();
    let one = json!({"event":"tools/call","name":"test.echo","run_id":"one"});
    assert_eq!(
        selectors::check(std::slice::from_ref(&one), &selector, Some("one")),
        Ok(())
    );
    assert!(
        selectors::check(&[one.clone(), one], &selector, Some("one"))
            .unwrap_err()
            .contains("observed 2")
    );
    assert!(
        selectors::check(
            &[json!({"event":"tools/call","name":"different","run_id":"one"})],
            &selector,
            Some("one")
        )
        .unwrap_err()
        .contains("observed 0")
    );
}

fn delegation_records() -> Vec<Value> {
    ["session.delegation.created", "tools/call", "session.commentary.append"]
        .into_iter()
        .map(|event| json!({"event":event,"run_id":"run-1","delegation_id":"d-1","name":"test.echo"}))
        .collect()
}

fn delegation_selectors() -> Vec<selectors::Selector> {
    selectors::parse(&[
        json!("session.delegation.created"),
        json!("tools/call {name: test.echo, count: 1}"),
        json!("session.commentary.append"),
    ])
    .unwrap()
}

#[test]
fn delegation_sequence_requires_the_same_nonempty_identity_at_every_step() {
    let selectors = delegation_selectors();
    assert_eq!(selectors::check(&delegation_records(), &selectors, None), Ok(()));
    for index in 0..3 {
        for value in [json!(null), json!(""), json!(42)] {
            let mut records = delegation_records();
            records[index]["delegation_id"] = value;
            assert_eq!(
                selectors::check(&records, &selectors, Some("run-1")),
                Err("trace expectation: missing delegation ID".into())
            );
        }
        let mut records = delegation_records();
        records[index]["delegation_id"] = json!("d-2");
        assert_eq!(
            selectors::check(&records, &selectors, Some("run-1")),
            Err("trace expectation: mismatched delegation ID".into())
        );
    }
}

#[test]
fn delegation_identity_is_scoped_to_the_selected_run_and_final_counts() {
    let mut records = delegation_records();
    let mut other = delegation_records();
    for record in &mut other {
        record["run_id"] = json!("run-2");
        record["delegation_id"] = json!("d-2");
    }
    records.extend(other);
    assert_eq!(
        selectors::check(&records, &delegation_selectors(), Some("run-1")),
        Ok(())
    );
    assert_eq!(
        selectors::check(&records, &delegation_selectors(), Some("run-2")),
        Ok(())
    );
    records.push(records[1].clone());
    assert_eq!(
        selectors::check(&records, &delegation_selectors(), Some("run-1")),
        Err("trace expectation 1 (tools/call): expected 1 occurrences, observed 2".into())
    );
}

#[test]
fn delegation_sequence_cannot_correlate_records_without_a_run() {
    let mut records = delegation_records();
    for record in &mut records {
        record.as_object_mut().unwrap().remove("run_id");
    }
    assert_eq!(
        selectors::check(&records, &delegation_selectors(), None),
        Err("trace expectation: missing run ID in delegation sequence".into())
    );
    for event in ["session.delegation.created", "session.commentary.append"] {
        let mut trace = Trace::default();
        assert_eq!(
            trace.append(format!("{}\n", json!({"event":event,"elapsed_ms":0,"delegation_id":"d-1"})).as_bytes()),
            Err("trace: missing run ID".into())
        );
        assert_eq!(trace.finish(), Err("trace: previously invalid trace".into()));
    }
}

#[test]
fn selectors_reject_empty_invalid_operators_and_non_scalar_values() {
    assert_eq!(selectors::parse(&[]).unwrap_err(), "trace expectation: empty selectors");
    for value in [
        "",
        "x {count: 0}",
        "x {count: -1}",
        "x {count: 1.5}",
        "x {count_gte: false}",
        "x {duration_gt: 1}",
        "x {event: y}",
        "x {name: []}",
        "x {name: null}",
        "x []",
        "x {",
        "x {name: {a: b}}",
    ] {
        assert!(selectors::Selector::parse(value).is_err(), "{value}");
    }
    assert_eq!(
        selectors::parse(&[json!(3)]).unwrap_err(),
        "trace selector: expected string"
    );
    assert!(selectors::check(&[], &[], None).unwrap_err().contains("empty"));
}

#[test]
fn incremental_trace_consumes_only_complete_lines_once_and_rejects_closed_partial() {
    let mut trace = Trace::default();
    let mut wire = serde_json::to_vec(&records()[0]).unwrap();
    wire.push(b'\n');
    trace.append(&wire[..7]).unwrap();
    assert_eq!(trace.records.len(), 0);
    assert_eq!(trace.finish(), Err("trace: incomplete final JSON line".into()));
    trace.append(&wire[7..]).unwrap();
    trace.append(&[]).unwrap();
    assert_eq!(trace.records, vec![records()[0].clone()]);
    assert_eq!(trace.finish(), Ok(()));
    assert_eq!(
        trace.append(b"invalid\n"),
        Err("trace: malformed complete JSON record".into())
    );
}

#[test]
fn trace_validation_enforces_timestamps_ids_and_resource_limit() {
    for (value, expected) in [
        (json!({}), "trace: missing event"),
        (json!({"event":"a","elapsed_ms":-1}), "trace: invalid timestamp"),
        (
            json!({"event":"wyoming.mic_frames","elapsed_ms":0}),
            "trace: missing run ID",
        ),
        (json!({"event":"a","elapsed_ms":0,"run_id":4}), "trace: invalid run ID"),
    ] {
        let mut trace = Trace::default();
        assert_eq!(trace.append(format!("{value}\n").as_bytes()), Err(expected.into()));
    }
    let mut trace = Trace::default();
    trace
        .append(b"{\"event\":\"a\",\"elapsed_ms\":5,\"run_id\":\"one\"}\n")
        .unwrap();
    trace
        .append(b"{\"event\":\"a\",\"elapsed_ms\":1,\"run_id\":\"two\"}\n")
        .unwrap();
    assert_eq!(
        trace.append(b"{\"event\":\"a\",\"elapsed_ms\":4,\"run_id\":\"one\"}\n"),
        Err("trace: backwards timestamp".into())
    );
    assert_eq!(
        Trace::default().append(&vec![b' '; 1024 * 1024 + 1]),
        Err("trace: exceeds 1 MiB limit".into())
    );
}

#[test]
fn latest_screen_requires_typed_state_and_exact_selected_fields() {
    assert_eq!(
        observers::screen(br#"{"state":"speaking"}"#, &json!({"state":"speaking"})),
        Ok(())
    );
    for bytes in [
        br#"{"state":"listening"}"#.as_slice(),
        b"{}",
        b"invalid",
        br#"{"state":"ready"}"#,
    ] {
        assert!(observers::screen(bytes, &json!({"state":"speaking"})).is_err());
    }
    assert!(
        observers::screen(br#"{"state":"speaking"}"#, &json!({"state":"speaking","unknown":1}))
            .unwrap_err()
            .contains("did not match")
    );
    assert!(
        observers::screen(br#"{"state":"speaking"}"#, &json!({}))
            .unwrap_err()
            .contains("requires state")
    );
    assert_eq!(
        observers::screen(
            br#"{"state":"ready","message":"test"}"#,
            &json!({"state":"ready","message":"test"})
        ),
        Ok(())
    );
}

#[test]
fn budgets_use_nearest_rank_and_only_current_run_samples() {
    for (sample, passes) in [(1499, true), (1500, true), (1501, false)] {
        let values = vec![
            json!({"event":"timing","name":"first_audio_after_endpoint_ms","run_id":"one","duration_ms":sample}),
            json!({"event":"timing","name":"first_audio_after_endpoint_ms","run_id":"other","duration_ms":9999}),
        ];
        assert_eq!(
            observers::budget(&values, "first_audio_after_endpoint_ms_p95", 1500.0, Some("one")).is_ok(),
            passes
        );
    }
    let values = (1..=20)
        .map(|n| json!({"event":"timing","name":"first_audio_after_endpoint_ms","run_id":"one","duration_ms":n}))
        .collect::<Vec<_>>();
    assert_eq!(
        observers::budget(&values, "first_audio_after_endpoint_ms_p95", 19.0, Some("one")),
        Ok(())
    );
    assert_eq!(
        observers::budget(&values, "first_audio_after_endpoint_ms_p95", 18.0, Some("one")),
        Err("budget: p95 exceeded ceiling".into())
    );
    assert_eq!(
        observers::budget(&[], "unknown", 1.0, Some("one")),
        Err("budget: unsupported metric".into())
    );
    assert_eq!(
        observers::budget(&[], "first_audio_after_endpoint_ms_p95", 1.0, Some("one")),
        Err("budget: no timing samples for current run".into())
    );
    assert!(
        observers::budget(&values, "first_audio_after_endpoint_ms_p95", f64::NAN, Some("one"))
            .unwrap_err()
            .contains("invalid ceiling")
    );
    assert!(
        observers::budget(
            &[json!({"event":"timing","name":"first_audio_after_endpoint_ms","run_id":"one","duration_ms":-1})],
            "first_audio_after_endpoint_ms_p95",
            1.0,
            Some("one")
        )
        .unwrap_err()
        .contains("invalid timing")
    );
}

#[test]
fn transcripts_are_correlated_and_do_not_enter_error_text() {
    let values = vec![
        json!({"event":"live.transcript","run_id":"one","text":"TEST echo [a.*]"}),
        json!({"event":"live.transcript","run_id":"other","text":"wrong"}),
    ];
    let text = observers::transcript(&values, Some("one")).unwrap();
    assert_eq!(
        super::super::run_oracles::speech(text.as_bytes(), &["test".into(), "[a.*]".into()]),
        Ok(())
    );
    assert_eq!(
        observers::transcript(&values, Some("missing")),
        Err("speech expectation: missing transcript for current run".into())
    );
    assert_eq!(
        super::super::run_oracles::speech(text.as_bytes(), &["absent".into()]),
        Err("speech expectation: recorded reply did not match".into())
    );
}

#[tokio::test(start_paused = true)]
async fn pending_work_times_out_on_injected_clock_and_cancellation_wins() {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let task = tokio::spawn(super::controlled(
        std::future::pending::<Result<(), String>>(),
        None,
        deadline,
    ));
    tokio::time::advance(std::time::Duration::from_secs(121)).await;
    assert_eq!(task.await.unwrap(), Err("sim: stage timed out".into()));
    let path = std::env::temp_dir().join(format!("lab-cancel-{}", std::process::id()));
    // The work would finish on its second poll. Cancellation requested during
    // the first poll must land before that, not up to one timer tick later.
    let mut polls = 0;
    let work = std::future::poll_fn(|context| {
        polls += 1;
        if polls > 1 {
            return std::task::Poll::Ready(Ok(()));
        }
        std::fs::write(&path, b"cancel").unwrap();
        context.waker().wake_by_ref();
        std::task::Poll::Pending
    });
    let later = deadline + std::time::Duration::from_secs(120);
    let result = super::controlled(work, Some(&path), later).await;
    assert_eq!((result, polls), (Err("sim: cancelled".into()), 1));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn microphone_counts_are_unsigned_observations_and_invalidity_is_sticky() {
    for count in [json!(null), json!(-1), json!(1.5), json!(true), json!("10")] {
        let mut record = records()[2].clone();
        record["count"] = count;
        let mut trace = Trace::default();
        assert!(trace.append(format!("{record}\n").as_bytes()).is_err());
        assert!(trace.append(b"").is_err());
        assert!(trace.finish().is_err());
    }
    let mut missing = records()[2].clone();
    missing.as_object_mut().unwrap().remove("count");
    assert!(Trace::default().append(format!("{missing}\n").as_bytes()).is_err());
    for count in [0, 10] {
        let mut record = records()[2].clone();
        record["count"] = json!(count);
        let mut trace = Trace::default();
        trace.append(format!("{record}\n").as_bytes()).unwrap();
        assert_eq!(trace.finish(), Ok(()));
        assert_eq!(trace.records[0]["count"], count);
    }
}

#[test]
fn muted_frame_oracle_accepts_only_measured_zero_and_rejects_one_missing_stale_or_duplicate() {
    let selectors = selectors::parse(&[json!(
        "mic.mute_window {window_id: mute-2, received_frames: 0, forwarded_frames: 0}"
    )])
    .unwrap();
    let zero = json!({"event":"mic.mute_window","window_id":"mute-2","received_frames":0,"forwarded_frames":0});
    assert_eq!(
        selectors::check(std::slice::from_ref(&zero), &selectors, Some("previous-run")),
        Ok(())
    );
    for field in ["received_frames", "forwarded_frames"] {
        let mut one = zero.clone();
        one[field] = json!(1);
        let error = selectors::check(&[one], &selectors, None).unwrap_err();
        assert!(
            error.contains(field) && error.contains("expected 0, observed 1"),
            "{error}"
        );
        let mut absent = zero.clone();
        absent.as_object_mut().unwrap().remove(field);
        assert!(
            selectors::check(&[absent], &selectors, None)
                .unwrap_err()
                .contains("missing or invalid")
        );
    }
    assert!(
        selectors::check(&[], &selectors, None)
            .unwrap_err()
            .contains("observed 0")
    );
    let mut stale = zero.clone();
    stale["window_id"] = json!("mute-1");
    assert!(
        selectors::check(&[stale], &selectors, None)
            .unwrap_err()
            .contains("observed 0")
    );
    assert!(
        selectors::check(&[zero.clone(), zero], &selectors, None)
            .unwrap_err()
            .contains("observed 2")
    );
    for invalid in [
        "mic.mute_window {}",
        "mic.mute_window {window_id: mute-2, received_frames: 0}",
        "mic.mute_window {window_id: mute-2, received_frames: 0, forwarded_frames: 1}",
    ] {
        assert!(selectors::Selector::parse(invalid).is_err());
    }
}

#[test]
fn restart_capability_bounds_count_process_and_fixture_before_launch() {
    let scenario = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/scenarios/m1-hassmic-bridge.yaml"
    );
    for process in ["bridge", "hassmic-host"] {
        let value = json!({"restart":{"process":process,"count":10,"say":"../fixtures/lab/w1-utterance.pcm"}});
        assert!(matches!(
            super::parse_step(&value, scenario).unwrap(),
            super::Step::Restart { count: 10, .. }
        ));
        for count in [0, 11] {
            let mut invalid = value.clone();
            invalid["restart"]["count"] = json!(count);
            assert_eq!(
                super::parse_step(&invalid, scenario).err().unwrap(),
                "restart: count must be 1..10"
            );
        }
        let mut missing = value;
        missing["restart"]["say"] = json!("missing.pcm");
        assert_eq!(
            super::parse_step(&missing, scenario).err().unwrap(),
            "sim say: missing PCM fixture"
        );
    }
    assert_eq!(
        super::parse_step(&json!({"restart":{"process":"other"}}), scenario)
            .err()
            .unwrap(),
        "restart: unsupported process"
    );
}
