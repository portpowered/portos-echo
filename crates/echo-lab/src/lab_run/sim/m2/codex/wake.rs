//! T3 configured runtime: prebuilt bridge, real pin/media, controlled device.
//! The native M2 scenario separately owns finite hassmic-host capture proof.
use super::*;
use std::io::Write;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_configured_wake_earcon_keeps_reply_and_mic_accounting_identical_when_disabled() {
    let mut reports = Vec::new();
    for enabled in [false, true] {
        let report = tokio::time::timeout(DEADLINE, Box::pin(exercise(enabled)))
            .await
            .unwrap();
        reports.push(report);
    }
    assert_eq!(reports[0]["microphone"], reports[1]["microphone"]);
    assert_eq!(reports[0]["reply_bytes"], reports[1]["reply_bytes"]);
    writeln!(std::io::stdout(), "T3-WAKE configured {}", json!(reports)).unwrap();
}

async fn exercise(enabled: bool) -> Value {
    let options = super::super::tests::options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (wake, requested) = oneshot::channel();
    let satellite = tokio::spawn(tests::satellite_configured(listener, requested, enabled));
    let mut state = Case {
        earcon: enabled,
        ..Case::default()
    };
    let started = state.start_bridge(&options, port).await;
    if started.is_err() {
        let cleanup = state.close().await;
        satellite.abort();
        let _ = satellite.await;
        assert_eq!(started, Ok(()), "trace={:?}; cleanup={cleanup:?}", state.trace.records);
        return Value::Null;
    }
    wake.send(()).unwrap();
    let settled = state.settle().await;
    let timing = state.relay.as_ref().unwrap().timing();
    let observed = state.close().await.unwrap();
    assert_eq!(settled, Ok(()), "{:?}", state.trace.records);
    assert!(provider::valid(&observed), "{observed}");
    assert_eq!(satellite.await.unwrap().0, 10);
    assert_eq!(timing.played, 9600);
    let mut report = report(&state, timing.played, enabled);
    report["wire"] = observe_wire(&timing, enabled);
    report
}

fn observe_wire(timing: &crate::lab_run::sim::m4::wire::Timing, enabled: bool) -> Value {
    let wire = timing.wake_observation();
    assert_eq!(wire["earcon_bytes"], if enabled { 7680 } else { 0 });
    if enabled {
        assert!(wire["wake_to_earcon_ms"].as_u64().unwrap() <= 100, "{wire}");
    } else {
        assert!(wire["wake_to_earcon_ms"].is_null());
    }
    wire
}

fn report(state: &Case, reply_bytes: usize, enabled: bool) -> Value {
    let chimes: Vec<_> = state
        .trace
        .records
        .iter()
        .filter(|row| row["event"] == "wake.earcon_played")
        .collect();
    assert_eq!(chimes.len(), usize::from(enabled));
    if enabled {
        assert_eq!(chimes[0]["duration_ms"], 160);
        assert_eq!(chimes[0]["run_id"], "run-1");
    }
    let pcm = state.clock["earcon_pcm"].as_array().unwrap();
    assert_eq!(pcm.len(), if enabled { 8 } else { 0 });
    let elapsed = |name| {
        state.trace.records.iter().find(|row| row["event"] == name).unwrap()["elapsed_ms"]
            .as_u64()
            .unwrap()
    };
    let connection_offset = elapsed("call.live_connected") - elapsed("run.started");
    if enabled {
        assert!(pcm[0]["epoch_ms"].as_u64().unwrap() < elapsed("call.live_connected"));
        let connected = state.clock["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["state"]["state"] == "connected")
            .unwrap();
        assert!(connected["epoch_ms"].as_u64().unwrap() >= elapsed("wake.earcon_played"));
    }
    let frames = state.frames.lock().unwrap().clone();
    oracle::screen(&json!({"frames":frames}), &oracle::expected("success")).unwrap();
    let microphone = state
        .trace
        .records
        .iter()
        .find(|row| row["event"] == "mic.completed")
        .unwrap();
    json!({"enabled":enabled,"earcon_frames":pcm.len(),"reply_bytes":reply_bytes,
        "wake_to_connected_logical_ms":connection_offset,
        "microphone":{"count":microphone["count"],"dropped_chunks":microphone["dropped_chunks"],
            "tail_samples":microphone["tail_samples"]},"clock":state.clock})
}

pub(super) async fn acknowledge(stream: &mut TcpStream) {
    let (mut frames, mut continued) = (0, false);
    loop {
        let (header, bytes) = tests::read(stream).await;
        match header["type"].as_str().unwrap() {
            "audio-start" => assert_eq!(header["data"], json!({"rate":24000,"width":2,"channels":1})),
            "audio-chunk" => {
                assert_eq!(bytes.len(), 960);
                frames += 1;
            }
            "hassmic-continue" => {
                assert!(!continued);
                continued = true;
            }
            "audio-stop" => {
                assert!(continued);
                assert_eq!(frames, 8);
                tests::send(stream, "played", json!({}), vec![]).await;
                return;
            }
            other => assert_eq!(other, "expected earcon playback"),
        }
    }
}
