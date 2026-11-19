//! Component T3: an owned inert process gates completion on an explicit signal.
//! Private trace snapshots supply observations; no device or provider is used.
use super::*;
use crate::lab_run::{run_report::Artifacts, sim::process::Process};
use std::io::Write;

fn append(path: &std::path::Path, records: &[Value]) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for record in records {
        writeln!(file, "{record}").unwrap();
    }
}

fn runtime(root: &std::path::Path, id: &str) -> Runtime {
    let path = root.join("trace");
    std::fs::write(&path, "").unwrap();
    let mut command = tokio::process::Command::new("python3");
    command.args(["-u", "-c",
        "import json,sys; sys.stdin.readline(); print(json.dumps({'event':'bridge.run_completed','run_id':sys.argv[1]}),flush=True); sys.stdin.read()", id]);
    Runtime {
        bridge: Some(Process::start(&mut command).unwrap()),
        trace_path: path,
        ..Runtime::default()
    }
}

fn first(path: &std::path::Path) {
    append(
        path,
        &[
            json!({"event":"run.started","run_id":"run-1","elapsed_ms":1}),
            json!({"event":"wyoming.run_started","run_id":"run-1","elapsed_ms":1}),
            json!({"event":"reply.played","run_id":"run-1","elapsed_ms":2,"outcome":"played"}),
        ],
    );
}

#[tokio::test]
async fn t3_device_t5_session_replies_do_not_consume_completion_or_rearm_trace_window() {
    let root = tempfile::tempdir().unwrap();
    let mut state = runtime(root.path(), "run-1");
    state.arm_wake().unwrap();
    first(&state.trace_path);
    // The child cannot publish run_completed until the explicit send below.
    state.session_reply(1).await.unwrap();
    assert_eq!(state.run.as_deref(), Some("run-1"));
    assert!(state.wakes.is_empty());
    append(
        &state.trace_path,
        &[json!({"event":"reply.played","run_id":"run-1","elapsed_ms":3,"outcome":"played"})],
    );
    state.session_reply(2).await.unwrap();
    assert_eq!(state.wake_offset, 0);
    append(
        &state.trace_path,
        &[
            json!({"event":"wyoming.mic_frames","run_id":"run-1","elapsed_ms":4,"count":20}),
            json!({"event":"wyoming.run_ended","run_id":"run-1","elapsed_ms":5}),
        ],
    );
    state
        .bridge
        .as_mut()
        .unwrap()
        .send(&json!({"finish":true}))
        .await
        .unwrap();
    state.finish_session(2).await.unwrap();
    state
        .expect_trace(
            &[
                json!("run.started"),
                json!("reply.played {outcome: played, count: 2}"),
                json!("wyoming.run_ended"),
            ],
            2,
        )
        .unwrap();
    assert_eq!(state.wakes.len(), 1);
    assert_eq!(state.wakes[0]["mic_frames"], 20);
    state.bridge.as_mut().unwrap().close().await.unwrap();
    // Final collection must still invalidate an initially successful oracle.
    append(
        &state.trace_path,
        &[json!({"event":"reply.played","run_id":"run-1","elapsed_ms":6,"outcome":"played"})],
    );
    let mut artifacts = Artifacts::create(root.path().join("out")).unwrap();
    let error = state.collect(&mut artifacts).unwrap_err();
    assert!(error.contains("expected 2 occurrences, observed 3"), "{error}");
    assert_eq!(artifacts.checks["step_2"], false);
}

#[tokio::test]
async fn t3_device_t5_session_early_end_error_and_second_run_refuse_next_turn() {
    for event in ["wyoming.run_ended", "bridge.error", "run.started"] {
        let root = tempfile::tempdir().unwrap();
        let mut state = runtime(root.path(), "run-1");
        first(&state.trace_path);
        state.session_reply(1).await.unwrap();
        append(
            &state.trace_path,
            &[json!({"event":event,"run_id":"run-2","elapsed_ms":3})],
        );
        assert_eq!(
            state.session_reply(2).await,
            Err("device: session ended or failed before reply barrier".into())
        );
        state
            .bridge
            .as_mut()
            .unwrap()
            .send(&json!({"finish":true}))
            .await
            .unwrap();
        state.bridge.as_mut().unwrap().close().await.unwrap();
    }
}

#[tokio::test]
async fn t3_device_t5_session_completion_requires_same_run_and_final_mic_measurement() {
    for (completed, end, error) in [
        (
            "run-2",
            true,
            "device: unmet prerequisite: session completed run identity",
        ),
        (
            "run-1",
            false,
            "trace expectation 2 (wyoming.run_ended): expected 1 occurrences, observed 0",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut state = runtime(root.path(), completed);
        first(&state.trace_path);
        state.session_reply(1).await.unwrap();
        append(
            &state.trace_path,
            &[json!({"event":"wyoming.mic_frames","run_id":"run-1","elapsed_ms":3,"count":10})],
        );
        if end {
            append(
                &state.trace_path,
                &[json!({"event":"wyoming.run_ended","run_id":"run-1","elapsed_ms":4})],
            );
        }
        state
            .bridge
            .as_mut()
            .unwrap()
            .send(&json!({"finish":true}))
            .await
            .unwrap();
        assert_eq!(state.finish_session(1).await, Err(error.into()));
        assert!(state.wakes.is_empty());
        state.bridge.as_mut().unwrap().close().await.unwrap();
    }
}
