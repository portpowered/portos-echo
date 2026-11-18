//! T3 runner component: delivered bridge/pin/media, controlled Wyoming socket.
//! The separate native scenario remains the hassmic-host milestone proof.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use std::io::Write;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn m2_codex_owner_runs_real_pinned_media_and_settles_played_before_idle() {
    tokio::time::timeout(DEADLINE, Box::pin(exercise())).await.unwrap();
}

#[tokio::test]
async fn t3_m2_codex_provider_closes_before_a_call_without_waiting_for_an_offer() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let owner = provider::Provider::start(python).await.unwrap();
    let observed = tokio::time::timeout(DEADLINE, owner.close()).await.unwrap().unwrap();
    assert_eq!(
        observed,
        json!({"event":"provider.snapshot", "calls":[], "sidebands":0, "external":0, "refreshes":0})
    );
}

#[tokio::test]
async fn t3_m1_repeated_provider_quiesces_and_closes_without_an_admitted_call() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let owner = provider::Provider::start_m1(python).await.unwrap();
    tokio::time::timeout(DEADLINE, owner.quiesce()).await.unwrap().unwrap();
    let observed = tokio::time::timeout(DEADLINE, owner.close()).await.unwrap().unwrap();
    assert_eq!(
        observed,
        json!({"event":"provider.snapshot", "calls":0, "matching_calls":0,
        "sidebands":0, "external":0, "refreshes":0})
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn m1_codex_continuous_capture_reopens_calls_without_reply_media() {
    tokio::time::timeout(DEADLINE, continuous_capture()).await.unwrap();
}

async fn continuous_capture() {
    let options = super::super::tests::options();
    let root = config::Root::new().unwrap();
    let output = tempfile::tempdir().unwrap();
    let (listener, listener_lifetime) = super::lifecycle::observed_listener().await;
    let address = listener.local_addr().unwrap();
    let provider = provider::Provider::start_m1(&options.python).await.unwrap();
    let mut command = continuous_command(
        options.bridge.as_ref().unwrap(),
        &root.0,
        output.path(),
        address,
        &provider.origin,
    );
    let (wake, requested) = tokio::sync::mpsc::channel(1);
    let satellite = tokio::spawn(continuous_satellite(listener, requested));
    let mut bridge = Process::start(&mut command).unwrap();
    bridge.expect("bridge.ready").await.unwrap();
    continuous_wakes(&mut bridge, &wake).await;
    assert!(listener_lifetime.upgrade().is_some());
    let closed = bridge.close().await;
    let observed = provider.close().await.unwrap();
    assert_eq!(closed, Ok(()));
    assert_eq!(satellite.await.unwrap(), 0);
    assert!(listener_lifetime.upgrade().is_none());
    assert_eq!(
        observed,
        json!({"event":"provider.snapshot", "calls":2,
        "matching_calls":2,"sidebands":2,"external":0,"refreshes":0})
    );
    let trace = read_trace(&output.path().join("bridge.private.jsonl")).unwrap();
    continuous_receipts(&trace);
}

async fn continuous_wakes(bridge: &mut Process, wake: &tokio::sync::mpsc::Sender<()>) {
    for index in 1..=2 {
        wake.send(()).await.unwrap();
        let completed = bridge.expect("bridge.run_completed").await.unwrap();
        assert_eq!(completed["run_id"], format!("run-{index}"));
        bridge.expect("bridge.ready").await.unwrap();
    }
}

pub(super) fn continuous_command(
    binary: &Path,
    root: &Path,
    output: &Path,
    address: std::net::SocketAddr,
    origin: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(binary);
    crate::lab_run::sim::production::configure_codex(
        &mut command,
        root,
        output,
        address,
        origin,
        "http://127.0.0.1:1/mcp",
    )
    .unwrap();
    // This component has no tool effects; the native M1 still owns host proof.
    let path = root.join("configured.private/bridge.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let start = text.find("[[mcp.server]]").unwrap();
    let end = start + text[start..].find("[screen]").unwrap();
    std::fs::write(
        path,
        format!("{}[mcp]\nserver = []\n\n{}", &text[..start], &text[end..]),
    )
    .unwrap();
    command
}

fn continuous_receipts(trace: &Trace) {
    let stops: Vec<_> = trace.records.iter().filter(|r| r["event"] == "live.closed").collect();
    assert_eq!(stops.len(), 2);
    assert!(stops.iter().all(|r| r["reason"] == "idle"));
    assert_eq!(
        trace
            .records
            .iter()
            .filter(|r| r["event"] == "wyoming.run_ended")
            .count(),
        2
    );
    let capture: Vec<_> = trace
        .records
        .iter()
        .filter(|r| r["event"] == "wyoming.mic_frames")
        .collect();
    assert_eq!(capture.len(), 2);
    for (index, receipt) in capture.iter().enumerate() {
        assert_eq!(receipt["run_id"], format!("run-{}", index + 1));
        assert!(receipt["count"].as_u64().unwrap() >= 10);
    }
}

async fn continuous_satellite(listener: Arc<TcpListener>, mut requested: tokio::sync::mpsc::Receiver<()>) -> usize {
    let mut stream = listener.accept().await.unwrap().0;
    assert_eq!(read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(read(&mut stream).await.0["type"], "describe");
    send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
    let wake: Value = serde_json::from_str(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/wyoming/wake-word.jsonl"
        ))
        .lines()
        .nth(1)
        .unwrap(),
    )
    .unwrap();
    let format = json!({"rate":16000,"width":2,"channels":1});
    let mut frames = 0;
    for _ in 0..2 {
        assert!(super::lifecycle::wait_for_wake(&mut stream, requested.recv()).await);
        send(&mut stream, "run-pipeline", wake["header"]["data"].clone(), vec![]).await;
        send(&mut stream, "audio-start", format.clone(), vec![]).await;
        let (returned, count) = continuous_playback(stream, format.clone()).await;
        stream = returned;
        frames += count;
    }
    assert!(!super::lifecycle::wait_for_wake(&mut stream, std::future::pending()).await);
    frames
}

pub(super) async fn continuous_playback(stream: TcpStream, format: Value) -> (TcpStream, usize) {
    let (mut input, mut output) = stream.into_split();
    // Device cadence continues through call negotiation and stops only when
    // the bridge ends the run; a fixed finite burst could precede media readiness.
    let mut cadence = tokio::time::interval(std::time::Duration::from_millis(20));
    let mut frames = 0;
    let mut replying = false;
    loop {
        let (row, bytes) = {
            let next = read(&mut input);
            tokio::pin!(next);
            loop {
                tokio::select! {
                    row = &mut next => break row,
                    _ = cadence.tick() => send(&mut output, "audio-chunk", format.clone(), vec![0;640]).await,
                }
            }
        };
        match row["type"].as_str().unwrap() {
            "describe" => send(&mut output, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "audio-start" => replying = true,
            "audio-chunk" => frames += 1,
            "hassmic-flush" | "hassmic-end" => {
                if continuous_control(row, bytes, &mut output, &mut replying).await {
                    break;
                }
            }
            "audio-stop" => {
                send(&mut output, "audio-stop", json!({}), vec![]).await;
                send(&mut output, "played", json!({}), vec![]).await;
                let expected = json!({"type":"hassmic-end","data":null,"data_length":0,"payload_length":0});
                assert_eq!(read(&mut input).await, (expected, vec![]));
                break;
            }
            other => assert_eq!(other, "expected reply end"),
        }
    }
    (input.reunite(output).unwrap(), frames)
}

async fn continuous_control(
    row: Value,
    bytes: Vec<u8>,
    output: &mut tokio::net::tcp::OwnedWriteHalf,
    replying: &mut bool,
) -> bool {
    let ended = row["type"] == "hassmic-end";
    let kind = if ended { "hassmic-end" } else { "hassmic-flush" };
    let expected = json!({"type":kind,"data":null,"data_length":0,"payload_length":0});
    assert_eq!((row, bytes), (expected, vec![]));
    if ended {
        send(output, "audio-stop", json!({}), vec![]).await;
    }
    if *replying {
        send(output, "played", json!({}), vec![]).await;
        *replying = false;
    }
    ended
}

#[tokio::test]
async fn t3_m2_codex_host_failure_names_the_dependency_without_echoing_payloads() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    for (event, expected) in [
        ("host.failed", "m2: pinned hassmic-host startup failed before readiness"),
        ("untrusted-payload", "m2: unexpected hassmic-host startup observation"),
    ] {
        let mut command = tokio::process::Command::new(python);
        command.args([
            "-u",
            "-c",
            "import json,sys; print(json.dumps({'event':sys.argv[1]}))",
            event,
        ]);
        let mut host = Process::start(&mut command).unwrap();
        assert_eq!(host_ready(&mut host).await, Err(expected.into()));
        assert_eq!(host.close().await, Ok(()));
    }
}

#[tokio::test]
async fn t3_m2_codex_missing_executable_reaps_provider_and_private_configuration() {
    let mut options = super::super::tests::options();
    let missing = tempfile::tempdir().unwrap();
    options.bridge = Some(missing.path().join("missing-bridge"));
    let mut state = Case::default();
    let result = state.start_bridge(&options, 1).await;
    assert_eq!(result, Err("launch: cannot spawn owned process".into()));
    let root = state.root.as_ref().unwrap().0.clone();
    let observed = tokio::time::timeout(DEADLINE, state.close()).await.unwrap().unwrap();
    assert_eq!(
        observed,
        json!({"event":"provider.snapshot", "calls":[], "sidebands":0, "external":0, "refreshes":0})
    );
    assert_eq!(state.trace.records, Vec::<Value>::new());
    drop(state);
    assert!(!root.exists());
}

#[tokio::test]
async fn t3_m2_codex_missing_host_preserves_partial_evidence_and_removes_case_directory() {
    let mut options = super::super::tests::options();
    let root = tempfile::tempdir().unwrap();
    options.python = root.path().join("missing-python").to_str().unwrap().into();
    let utterance = super::super::tests::utterance();
    let (measured, result) = run(&options, root.path(), &utterance).await;
    assert_eq!(result, Err("launch: cannot spawn owned process".into()));
    assert_eq!(measured["provider"], Value::Null);
    assert_eq!(measured["frames"], json!([]));
    assert_eq!(measured["trace"], json!([]));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn m2_finite_capture_preserves_the_nominated_waveform_in_native_host_blocks() {
    let path = super::super::tests::utterance();
    let original = std::fs::read(&path).unwrap();
    let pcm = finite_pcm(&path).unwrap();
    assert_eq!(pcm.len(), 3200);
    assert_eq!(pcm, original.repeat(5));
}

async fn exercise() {
    let options = super::super::tests::options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (wake, wake_requested) = oneshot::channel();
    let satellite = tokio::spawn(satellite(listener, wake_requested));
    let mut state = Case::default();
    state.start_bridge(&options, port).await.unwrap();
    wake.send(()).unwrap();
    let settled = state.settle().await;
    let timing = state.relay.as_ref().unwrap().timing();
    let observed = state.close().await.unwrap();
    assert_eq!(settled, Ok(()), "{:?}", state.trace.records);
    assert!(provider::valid(&observed), "{observed}");
    let (frames, energy) = satellite.await.unwrap();
    assert_eq!(frames, 10, "finite synthetic reply frames");
    assert!(energy > 480 * 1000_i64.pow(2), "decoded reply energy {energy}");
    let measured = json!({"case":"success", "runtime":"codex", "calls":1, "provider_wire":true, "refreshes":0,
        "trace":state.trace.records, "frames":state.frames.lock().unwrap().clone(),
        "reply_pcm":{"pcm_bytes":timing.played,"nonzero_samples":timing.nonzero_samples,
            "audible_samples":timing.audible_samples}});
    assert_eq!(oracle::check(&measured), Ok(()), "{measured}");
    let replies: Vec<_> = state
        .trace
        .records
        .iter()
        .filter(|r| r["event"] == "reply.played")
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["outcome"], "played");
    assert_eq!(replies[0]["frames"], frames);
    assert_eq!(replies[0]["dropped_frames"], 0);
}

pub(super) async fn satellite(listener: TcpListener, wake: oneshot::Receiver<()>) -> (usize, i64) {
    satellite_configured(listener, wake, false).await
}

pub(super) async fn satellite_configured(
    listener: TcpListener,
    wake: oneshot::Receiver<()>,
    earcon: bool,
) -> (usize, i64) {
    let mut stream = listener.accept().await.unwrap().0;
    drop(listener);
    assert_eq!(read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(read(&mut stream).await.0["type"], "describe");
    send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
    wake.await.unwrap();
    let row: Value = serde_json::from_str(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/wyoming/wake-word.jsonl"
        ))
        .lines()
        .nth(1)
        .unwrap(),
    )
    .unwrap();
    send(&mut stream, "run-pipeline", row["header"]["data"].clone(), vec![]).await;
    capture(&mut stream).await;
    if earcon {
        super::wake::acknowledge(&mut stream).await;
    }
    let result = playback(&mut stream).await;
    // Retain socket through bridge EOF; answer idle probes without another run.
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    result
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_mouth_sync_uses_completed_pcm_writes_and_confirmed_states_on_one_epoch() {
    tokio::time::timeout(DEADLINE, Box::pin(super::timing::exercise(false, false)))
        .await
        .unwrap();
}

async fn capture(stream: &mut TcpStream) {
    let format = json!({"rate":16000,"width":2,"channels":1});
    send(stream, "audio-start", format.clone(), vec![]).await;
    let waveform: [i16; 16] = [
        0, 6000, 11000, 14500, 16000, 14500, 11000, 6000, 0, -6000, -11000, -14500, -16000, -14500, -11000, -6000,
    ];
    for _ in 0..8 {
        send(
            stream,
            "audio-chunk",
            format.clone(),
            (0..320).flat_map(|n| waveform[n % 16].to_le_bytes()).collect(),
        )
        .await;
    }
    send(stream, "audio-stop", json!({}), vec![]).await;
}

async fn playback(stream: &mut TcpStream) -> (usize, i64) {
    let (mut frames, mut energy, mut nonzero, mut audible) = (0, 0, 0, 0);
    loop {
        let (header, bytes) = read(stream).await;
        match header["type"].as_str().unwrap() {
            "describe" => send(stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "audio-start" => assert_eq!(header["data"], json!({"rate":24000,"width":2,"channels":1})),
            "audio-chunk" => {
                assert_eq!(bytes.len(), 960);
                frames += 1;
                for sample in bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)) {
                    nonzero += usize::from(sample != 0);
                    audible += usize::from(sample.unsigned_abs() > 64);
                }
                energy += bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i64::from(i16::from_le_bytes(*b)).pow(2))
                    .sum::<i64>();
            }
            "audio-stop" => {
                assert!(frames >= 1);
                send(stream, "played", json!({}), vec![]).await;
                let expected = json!({"type":"hassmic-end","data":null,"data_length":0,"payload_length":0});
                assert_eq!(read(stream).await, (expected, vec![]));
                break;
            }
            other => assert_eq!(other, "expected reply audio", "unexpected Wyoming event"),
        }
    }
    writeln!(
        std::io::stdout(),
        "M2 reply: pcm_bytes={} nonzero_samples={nonzero} audible_samples={audible}",
        frames * 960
    )
    .unwrap();
    assert_eq!(frames, 10);
    assert!(nonzero >= 2400);
    assert!(audible >= 2400);
    (frames, energy)
}

pub(super) async fn send(stream: &mut (impl tokio::io::AsyncWrite + Unpin), kind: &str, data: Value, payload: Vec<u8>) {
    stream
        .write_all(&encode(&Event {
            header: Header {
                r#type: kind.into(),
                data: Some(data),
                data_length: 0,
                payload_length: payload.len(),
            },
            data: None,
            payload,
        }))
        .await
        .unwrap();
}

pub(super) async fn read(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> (Value, Vec<u8>) {
    let mut bytes = vec![];
    loop {
        let b = stream.read_u8().await.unwrap();
        bytes.push(b);
        assert!(bytes.len() < 8192);
        if b == b'\n' {
            break;
        }
    }
    let row: Value = serde_json::from_slice(&bytes).unwrap();
    let mut data = vec![0; usize::try_from(row["data_length"].as_u64().unwrap_or(0)).unwrap()];
    stream.read_exact(&mut data).await.unwrap();
    let mut payload = vec![0; usize::try_from(row["payload_length"].as_u64().unwrap_or(0)).unwrap()];
    stream.read_exact(&mut payload).await.unwrap();
    (row, payload)
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn m2_codex_negative_cases_reject_before_admission_and_release_private_homes() {
    for case in ["invalid", "expired"] {
        let options = super::super::tests::options();
        let (measured, result) = tokio::time::timeout(DEADLINE, rejection::run(&options, case))
            .await
            .unwrap();
        assert_eq!(result, Ok(()), "{measured}");
        assert_eq!(
            measured["refreshes"],
            if case == "expired" {
                oracle::EXPIRED_REFRESHES
            } else {
                0
            }
        );
        assert_eq!(measured["home_released"], true);
        assert_eq!(measured["auth_unchanged"], true);
        assert_eq!(measured["wyoming_admissions"], 0);
    }
}
