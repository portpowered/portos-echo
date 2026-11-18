//! T3 configured runtime: prebuilt bridge, real pin/media, controlled device.
//! The native M2 scenario separately owns finite hassmic-host capture proof.
use super::*;
use std::io::Write;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and pinned hassmic-host"]
async fn t3_native_wake_earcon_preserves_finite_mic_endpoint_and_reply() {
    let mut reports = Vec::new();
    for enabled in [false, true] {
        reports.push(tokio::time::timeout(DEADLINE, Box::pin(native(enabled))).await.unwrap());
    }
    assert_eq!(reports[0]["capture"], reports[1]["capture"]);
    assert_eq!(reports[0]["microphone"], reports[1]["microphone"]);
    writeln!(std::io::stdout(), "T3-WAKE native {}", json!(reports)).unwrap();
}

async fn native(enabled: bool) -> Value {
    let options = super::super::tests::options();
    let utterance = super::super::tests::utterance();
    let mut state = Case {
        earcon: enabled,
        fixture: Some(finite_pcm(&utterance).unwrap()),
        ..Case::default()
    };
    let work = async {
        state.start(&options).await?;
        state.exercise(&utterance).await
    }
    .await;
    let timing = state.relay.as_ref().map(crate::lab_run::sim::m4::wire::Relay::timing);
    let closed = state.close().await;
    assert_eq!(work, Ok(()), "trace={:?}; close={closed:?}", state.trace.records);
    assert!(provider::valid(&closed.unwrap()));
    let timing = timing.unwrap();
    assert_eq!(timing.played, 9600);
    let capture = timing.capture_observation();
    assert!(capture["endpoint_sample"].as_u64().is_some());
    assert_eq!(capture["earcon_first_frame_present"], false);
    let mut result = report(&state, timing.played, enabled);
    assert_eq!(result["microphone"]["dropped_chunks"], 0);
    assert_eq!(result["microphone"]["tail_samples"], 0);
    result["capture"] = capture;
    result["wire"] = observe_wire(&timing, enabled);
    result["host_reply"] = state.playback.clone();
    result
}

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

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_reuses_live_call_with_distinct_chimes_and_no_connected_republication() {
    tokio::time::timeout(
        DEADLINE,
        Box::pin(async {
            let mut owner = super::lifecycle::Owner::start(120, true).await;
            owner.run(1).await;
            owner.run(2).await;
            let trace = owner.trace();
            let chimes: Vec<_> = trace
                .records
                .iter()
                .filter(|r| r["event"] == "wake.earcon_played")
                .collect();
            assert_eq!(chimes.len(), 2);
            for (index, chime) in chimes.iter().enumerate() {
                assert_eq!(chime["run_id"], format!("run-{}", index + 1));
                assert_eq!(chime["duration_ms"], 160);
            }
            assert_eq!(
                trace
                    .records
                    .iter()
                    .filter(|r| r["event"] == "call.live_connected")
                    .count(),
                1
            );
            assert_eq!(trace.records.iter().filter(|r| r["event"] == "reply.played").count(), 2);
            writeln!(std::io::stdout(), "T3-WAKE reused {}", json!(chimes)).unwrap();
            owner.close(2).await;
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_call_refusal_and_bad_sdp_with_missing_played_cleanup_without_success() {
    tokio::time::timeout(
        DEADLINE,
        Box::pin(async {
            for fault in ["call_refused", "malformed_sdp"] {
                super::lifecycle::failed_call(fault, true).await;
                writeln!(
                    std::io::stdout(),
                    "T3-WAKE {fault}: no connected/chime/reply success; child, socket and home released"
                )
                .unwrap();
            }
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_active_shutdown_never_fabricates_success() {
    tokio::time::timeout(DEADLINE, Box::pin(interrupted("shutdown")))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_active_kill_never_fabricates_success() {
    tokio::time::timeout(DEADLINE, Box::pin(interrupted("kill")))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_active_mute_never_fabricates_success() {
    tokio::time::timeout(DEADLINE, Box::pin(interrupted("mute")))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_active_eof_never_fabricates_success() {
    tokio::time::timeout(DEADLINE, Box::pin(interrupted("eof")))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_wake_earcon_active_missing_played_never_fabricates_success() {
    tokio::time::timeout(DEADLINE, Box::pin(interrupted("missing_played")))
        .await
        .unwrap();
}

async fn interrupted(fault: &'static str) {
    let options = super::super::tests::options();
    let root = config::Root::new().unwrap();
    let output = tempfile::tempdir().unwrap();
    let (listener, lifetime) = super::lifecycle::observed_listener().await;
    let provider = provider::Provider::start_fault(&options.python, "pending_call")
        .await
        .unwrap();
    let binary = options.bridge.as_ref().unwrap();
    let mut command = tests::continuous_command(
        binary,
        &root.0,
        output.path(),
        listener.local_addr().unwrap(),
        &provider.origin,
    );
    let config = root.0.join("configured.private/bridge.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, text.replace("wake_earcon = false", "wake_earcon = true")).unwrap();
    let (wake, requested) = oneshot::channel();
    let (active, first_frame) = oneshot::channel();
    let satellite = tokio::spawn(interrupted_satellite(listener, requested, active, fault));
    let mut bridge = Process::start(&mut command).unwrap();
    let collected = bridge.observe_collection();
    bridge.expect("bridge.ready").await.unwrap();
    let path = output.path().join("bridge.private.jsonl");
    let mut screen = super::lifecycle::paired_screen(&path).await;
    wake.send(()).unwrap();
    first_frame.await.unwrap();
    assert_eq!(screen.next().await.unwrap()["state"]["state"], "listening");
    interrupt_control(binary, &root.0, &mut screen, fault).await;
    interrupted_cleanup(bridge, collected, fault).await;
    let frames = satellite.await.unwrap();
    interrupted_frames(fault, frames);
    let observed = provider.close().await.unwrap();
    interrupted_provider(&observed);
    interrupted_receipts(&path, fault);
    assert!(lifetime.upgrade().is_none());
    release_home(binary, root).await;
    writeln!(
        std::io::stdout(),
        "T3-WAKE {fault}: frames={frames}; no success; child/socket/home released"
    )
    .unwrap();
}

async fn release_home(binary: &Path, root: config::Root) {
    super::lifecycle::reopened_status(binary, &root.0).await;
    let home = root.0.clone();
    drop(root);
    assert!(!home.exists());
}

async fn interrupted_cleanup(
    mut bridge: Process,
    collected: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    fault: &str,
) {
    if !matches!(fault, "shutdown" | "kill") {
        assert_eq!(bridge.line().await, Err("process: exited before observation".into()));
    }
    let result = bridge.close().await;
    drop(bridge);
    let status = collected.try_recv().unwrap();
    if fault == "shutdown" {
        assert_eq!(result, Ok(()));
        assert!(status.success());
    } else {
        assert_eq!(result, Err("cleanup: owned child exited nonzero".into()));
        assert_eq!(status.code(), Some(1));
    }
}

async fn interrupted_satellite(
    listener: Arc<TcpListener>,
    wake: oneshot::Receiver<()>,
    active: oneshot::Sender<()>,
    fault: &str,
) -> usize {
    let mut stream = interrupted_open(listener, wake).await;
    assert_eq!(tests::read(&mut stream).await.0["type"], "audio-start");
    let (header, bytes) = tests::read(&mut stream).await;
    assert_eq!(header["type"], "audio-chunk");
    assert_eq!(bytes.len(), 960);
    active.send(()).unwrap();
    if fault == "eof" {
        return 1;
    }
    if fault == "mute" {
        tests::send(&mut stream, "hassmic-mute", json!({"muted":true}), vec![]).await;
    }
    drain_unacknowledged(&mut stream).await
}

async fn drain_unacknowledged(stream: &mut TcpStream) -> usize {
    let mut frames = 1;
    while stream.peek(&mut [0]).await.unwrap() != 0 {
        let (header, bytes) = tests::read(stream).await;
        if header["type"] == "audio-chunk" {
            assert_eq!(bytes.len(), 960);
            frames += 1;
        } else {
            assert_eq!(bytes, Vec::<u8>::new());
        }
        // Deliberately withhold Played, including after a cut. The owner's
        // real settlement deadline must collect an unacknowledged operation.
    }
    frames
}

async fn interrupt_control(binary: &Path, root: &Path, screen: &mut Screen, fault: &str) {
    if fault == "mute" {
        assert_eq!(screen.next().await.unwrap()["state"]["state"], "muted");
    }
    if fault == "kill" {
        let result = tokio::process::Command::new(binary)
            .current_dir(root.join("configured.private"))
            .args(["kill", "--config", "bridge.toml"])
            .output()
            .await
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"kill-switch: on\n");
        assert_eq!(result.stderr, b"");
        assert_eq!(screen.next().await.unwrap()["state"]["state"], "error");
    }
}

fn interrupted_receipts(path: &Path, fault: &str) {
    let trace = read_trace(path).unwrap();
    for event in [
        "wake.earcon_played",
        "reply.started",
        "reply.played",
        "bridge.run_completed",
    ] {
        assert_eq!(
            trace.records.iter().filter(|r| r["event"] == event).count(),
            0,
            "{fault}: {event}"
        );
    }
    assert!(!std::fs::read_to_string(path).unwrap().contains("synthetic private"));
}

async fn interrupted_open(listener: Arc<TcpListener>, wake: oneshot::Receiver<()>) -> TcpStream {
    let mut stream = listener.accept().await.unwrap().0;
    assert_eq!(tests::read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(tests::read(&mut stream).await.0["type"], "describe");
    tests::send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
    wake.await.unwrap();
    tests::send(
        &mut stream,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
        vec![],
    )
    .await;
    tests::send(
        &mut stream,
        "audio-start",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![],
    )
    .await;
    stream
}

fn interrupted_provider(observed: &Value) {
    assert_eq!(observed["external"], 0);
    assert_eq!(observed["refreshes"], 0);
}

fn interrupted_frames(fault: &str, frames: usize) {
    let maximum = if fault == "mute" { 7 } else { 8 };
    assert!((1..=maximum).contains(&frames), "{fault}: {frames}");
}
