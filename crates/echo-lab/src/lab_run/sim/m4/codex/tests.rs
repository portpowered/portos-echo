//! T3 component: same canonical runner, real pin/media/MCP, controlled Wyoming.
//! The complete milestone still requires the native hassmic-host scenario.
use super::*;
#[path = "phase.rs"]
mod phase;
#[path = "thinking.rs"]
mod thinking;
use echo_contracts::wyoming::{Event, Header, encode};
use std::io::Write;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

#[test]
fn failed_journey_retains_primary_error_exit_and_independent_observations() {
    let mut case = Case {
        bridge_exit: Some("exit status: 1; forced=false".into()),
        snapshot: json!({"models":[{"outputs":0}],"appends":[],"transcripts":[]}),
        clock: json!({"epoch_ms":480,"pcm":[],"states":[]}),
        ..Case::default()
    };
    case.trace.records.push(json!({"event":"tools/call","elapsed_ms":480}));
    let error = case.failure("process: exited before observation; cleanup: owned child exited nonzero");
    let (primary, observed) = error.split_once("; m4 failure observations: ").unwrap();
    assert_eq!(
        primary,
        "process: exited before observation; cleanup: owned child exited nonzero"
    );
    let observed: Value = serde_json::from_str(observed).unwrap();
    assert_eq!(observed["bridge_exit"], "exit status: 1; forced=false");
    assert_eq!(observed["bridge_trace"], json!(case.trace.records));
    assert_eq!(observed["provider"], case.snapshot);
    assert_eq!(observed["provider_expected"], receipts::expected(false, false));
    assert_eq!(observed["playout_clock"], case.clock);
    assert!(
        case.failure("cancelled")
            .starts_with("cancelled; m4 failure observations: ")
    );
}

#[test]
fn paired_sequence_rejects_a_discarded_late_mouth_interval() {
    let states: Vec<_> = ["ready", "speaking", "ready", "speaking", "ready"]
        .iter()
        .enumerate()
        .map(|(index, state)| json!({"epoch_ms":index * 20,"state":{"state":state}}))
        .collect();
    let frames: Vec<_> = states
        .iter()
        .enumerate()
        .map(|(index, stamp)| json!({"seq":index + 1,"state":stamp["state"]}))
        .collect();
    let clock = json!({"states":states});
    assert_eq!(receipts::screen(&frames, &clock), Ok(()));
    assert_eq!(
        receipts::screen(&frames[..3], &clock),
        Err("m4: incomplete paired screen sequence".into())
    );
    let mut wrong = frames;
    wrong[3]["state"] = json!({"state":"ready"});
    assert_eq!(
        receipts::screen(&wrong, &clock),
        Err("m4: paired screen/clock sequence mismatch".into())
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m4_canonical_provider_matches_real_pin_tools_media_and_native_receipts() {
    exercise(false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m4_canonical_provider_reports_mcp_failure_with_one_native_append() {
    exercise(true).await;
}

async fn exercise(negative: bool) {
    exercise_mode(negative, false).await;
}

async fn exercise_mode(negative: bool, emotion: bool) {
    let measurement = phase::Measurement::new(negative, emotion);
    let phases = phase::Phase::new("bridge ready", phase::STARTUP);
    let options = options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (wake, awakened) = oneshot::channel();
    let mut satellite = tokio::spawn(satellite(listener, awakened, measurement, phases.clone()));
    let mut case = Case {
        emotion,
        ..Case::default()
    };
    let observed = phases
        .wait(async {
            case.connect(&options, negative, address).await?;
            phases.seen("bridge.ready");
            measurement.mark("bridge ready");
            let path = &case.root.as_ref().unwrap().0;
            let mut screen = Screen::connect(path).await?;
            screen.expect("ready").await?;
            phases.seen("screen.ready");
            wake.send(()).map_err(|()| "satellite: wake receiver closed")?;
            phases.next("provider/session ready", phase::STARTUP);
            work(&mut case, &mut screen, negative, emotion, measurement, &phases).await
        })
        .await;
    let path = case.root.as_ref().map(|root| root.0.clone());
    let timing = case.relay.as_ref().map(Relay::timing);
    phases.next("receipts collected", phase::CLEANUP);
    let closed = phases.wait(case.close()).await;
    if closed.is_ok() {
        measurement.mark("receipts collected");
    }
    let result = phase::finish(observed, closed);
    if result.is_err() {
        satellite.abort();
    }
    let joined = phases
        .wait(async { (&mut satellite).await.map_err(|_| "satellite: task failed".to_owned()) })
        .await;
    satellite.abort();
    assert!(result.is_ok(), "work: {result:?}; satellite: {joined:?}");
    let (completion, frames) = result.unwrap();
    assert_eq!(completion, json!({"event":"bridge.run_completed","run_id":"run-1"}));
    assert_results(&case, &timing.unwrap(), negative, emotion);
    assert_eq!(joined.unwrap(), 9600);
    assert_presentation(&frames, negative, emotion);
    assert!(!path.unwrap().exists());
}

async fn work(
    case: &mut Case,
    screen: &mut Screen,
    negative: bool,
    emotion: bool,
    measurement: phase::Measurement,
    phases: &phase::Phase,
) -> Result<(Value, Vec<Value>), String> {
    let (connected, ready) = oneshot::channel();
    let replies = case.provider.as_ref().unwrap().commands.clone();
    tokio::try_join!(
        settle(
            case.bridge.as_mut().unwrap(),
            case.provider.as_mut().unwrap(),
            case.relay.as_ref().unwrap(),
            ready,
            &mut case.clock
        ),
        presentation(
            screen,
            case.spoof.as_ref().unwrap(),
            (negative, emotion),
            connected,
            replies,
            (measurement, phases)
        )
    )
}

fn assert_presentation(frames: &[Value], negative: bool, emotion: bool) {
    if emotion && !negative {
        assert_animation(frames.to_vec());
    }
    if emotion && negative {
        let states: Vec<_> = frames.iter().map(|f| f["state"].clone()).collect();
        assert_eq!(
            states,
            vec![
                json!({"state":"listening"}),
                json!({"state":"connected"}),
                json!({"state":"thinking"}),
                json!({"state":"speaking"}),
                json!({"state":"connected"}),
                json!({"state":"ready","message":"Echo Live ready"})
            ]
        );
    }
}

async fn satellite(
    listener: TcpListener,
    wake: oneshot::Receiver<()>,
    measurement: phase::Measurement,
    phases: phase::Phase,
) -> usize {
    let mut socket = wake_satellite(listener, wake, &phases).await;
    let count = playback(&mut socket, &phases).await;
    phases.seen("satellite.played");
    measurement.mark("media done");
    assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
    count
}

async fn wake_satellite(listener: TcpListener, wake: oneshot::Receiver<()>, phases: &phase::Phase) -> TcpStream {
    let mut socket = listener.accept().await.unwrap().0;
    assert_eq!(read(&mut socket).await.0["type"], "run-satellite");
    assert_eq!(read(&mut socket).await.0["type"], "describe");
    phases.seen("satellite.describe");
    send(&mut socket, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
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
    send(&mut socket, "run-pipeline", row["header"]["data"].clone(), vec![]).await;
    capture(&mut socket).await;
    socket
}
async fn playback(socket: &mut TcpStream, phases: &phase::Phase) -> usize {
    let mut pcm = vec![];
    loop {
        let (header, bytes) = read(socket).await;
        match header["type"].as_str().unwrap() {
            "describe" => send(socket, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "audio-start" => assert_eq!(header["data"], json!({"rate":24000,"width":2,"channels":1})),
            "audio-chunk" => {
                assert_eq!(bytes.len(), 960);
                pcm.extend(bytes);
                phases.seen("satellite.audio-chunk");
            }
            "audio-stop" => {
                send(socket, "played", json!({}), vec![]).await;
                let expected = json!({"type":"hassmic-end","data":null,"data_length":0,"payload_length":0});
                assert_eq!(read(socket).await, (expected, vec![]));
                break;
            }
            // Display only: the dispatch runs before, and between, reply audio.
            "hassmic-thinking" => assert!(header["data"]["thinking"].is_boolean()),
            other => assert_eq!(other, "expected reply audio"),
        }
    }
    assert_eq!(pcm.len(), 9600);
    let samples: Vec<_> = pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
    let nonzero = samples.iter().filter(|&&s| s != 0).count();
    let audible = samples.iter().filter(|&&s| s.unsigned_abs() > 64).count();
    writeln!(
        std::io::stdout(),
        "M4 reply: pcm_bytes={} nonzero_samples={nonzero} audible_samples={audible}",
        pcm.len()
    )
    .unwrap();
    assert!(nonzero >= 2400);
    assert!(audible >= 2400);
    pcm.len()
}
async fn send(socket: &mut TcpStream, kind: &str, data: Value, payload: Vec<u8>) {
    socket
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
async fn read(socket: &mut TcpStream) -> (Value, Vec<u8>) {
    let mut bytes = vec![];
    loop {
        let byte = socket.read_u8().await.unwrap();
        bytes.push(byte);
        assert!(bytes.len() < 8192);
        if byte == b'\n' {
            break;
        }
    }
    let header: Value = serde_json::from_slice(&bytes).unwrap();
    let mut data = vec![0; usize::try_from(header["data_length"].as_u64().unwrap_or(0)).unwrap()];
    socket.read_exact(&mut data).await.unwrap();
    let mut payload = vec![0; usize::try_from(header["payload_length"].as_u64().unwrap_or(0)).unwrap()];
    socket.read_exact(&mut payload).await.unwrap();
    (header, payload)
}

pub(super) fn options() -> Options {
    let binary = std::env::current_exe()
        .unwrap()
        .ancestors()
        .find(|p| matches!(p.file_name().and_then(|n| n.to_str()), Some("deps" | "build")))
        .and_then(Path::parent)
        .unwrap()
        .join("echo-bridge");
    crate::lab_run::run_options::parse(&[
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/scenarios/m4-spoof-tool.yaml")
            .to_string_lossy()
            .into_owned(),
        "--driver".into(),
        "sim".into(),
        "--bridge".into(),
        binary.to_string_lossy().into_owned(),
    ])
    .unwrap()
}
async fn presentation(
    screen: &mut Screen,
    spoof: &Spoof,
    mode: (bool, bool),
    connected: oneshot::Sender<()>,
    replies: tokio::sync::mpsc::Sender<Control>,
    timing: (phase::Measurement, &phase::Phase),
) -> Result<Vec<Value>, String> {
    let (measurement, phases) = timing;
    let (negative, emotion) = mode;
    let mut states = vec![];
    observe(screen, &mut states, "listening").await?;
    phases.seen("screen.listening");
    observe(screen, &mut states, "connected").await?;
    phases.seen("screen.connected");
    measurement.mark("provider/session ready");
    phases.next("tools listed", Duration::from_secs(1));
    // Catalog discovery precedes session startup. This is an observation upper
    // bound at connected, not an invented tools/list arrival timestamp.
    if spoof
        .snapshot()
        .map_err(|_| "m4: missing MCP timing observation")?
        .requests
        .iter()
        .any(|request| request.method == "tools/list")
    {
        phases.seen("mcp.tools/list");
        measurement.mark("tools listed (observed by connected)");
    } else {
        return Err("m4: missing MCP tools/list observation".into());
    }
    phases.next("media done", phase::MEDIA);
    if negative {
        spoof.set_fault(Fault::Http500).unwrap();
    }
    connected.send(()).unwrap();
    observe(screen, &mut states, "thinking").await?;
    if emotion {
        if !negative {
            observe(screen, &mut states, "emotion").await?;
            observe(screen, &mut states, "thinking").await?;
        }
        replies.send(Control::Reply).await.unwrap();
    }
    observe(screen, &mut states, "speaking").await?;
    phases.seen("screen.speaking");
    observe(screen, &mut states, "connected").await?;
    observe(screen, &mut states, "ready").await?;
    phases.seen("screen.ready");
    Ok(states)
}
fn stages(trace: &Trace) {
    let stages: Vec<_> = trace
        .records
        .iter()
        .filter(|r| {
            // Only the independent provider receipt proves native creation.
            if r["event"] == "delegation.created" {
                return r["source"] == "provider.sideband";
            }
            matches!(
                r["event"].as_str(),
                Some("delegation.created" | "item/tool/call" | "tools/call" | "delegation.context.append")
            )
        })
        .map(|r| r["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        stages,
        [
            "delegation.created",
            "item/tool/call",
            "tools/call",
            "delegation.context.append"
        ]
    );
}

/// The production bridge's own budget sample, one per reply. This provider
/// sends the user transcript after the reply, so the delegated turn's start
/// is the endpoint; the scenario's 1500 ms budget bounds a plausible value.
fn first_audio(trace: &Trace) {
    let samples: Vec<_> = trace.records.iter().filter(|r| r["event"] == "timing").collect();
    assert_eq!(samples.len(), 1, "{samples:?}");
    assert_eq!(
        (&samples[0]["name"], &samples[0]["run_id"], &samples[0]["endpoint"]),
        (
            &json!("first_audio_after_endpoint_ms"),
            &json!("run-1"),
            &json!("delegated_turn")
        )
    );
    let duration = samples[0]["duration_ms"].as_u64();
    assert!(duration.is_some_and(|ms| ms <= 1500), "{}", samples[0]);
}

async fn capture(socket: &mut TcpStream) {
    let format = json!({"rate":16000,"width":2,"channels":1});
    send(socket, "audio-start", format.clone(), vec![]).await;
    for chunk in world::fixture().chunks(640) {
        send(socket, "audio-chunk", format.clone(), chunk.to_vec()).await;
    }
    send(socket, "audio-stop", json!({}), vec![]).await;
}
fn text(negative: bool) -> &'static str {
    if negative {
        super::super::FALLBACK
    } else {
        super::super::SUCCESS
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m6_canonical_provider_preserves_exact_animation_transcript_and_pcm() {
    exercise_mode(false, true).await;
}

fn assert_animation(frames: Vec<Value>) {
    let mut states = vec![json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}})];
    states.extend(frames);
    assert_eq!(
        super::super::emotion::ordered(&states, &json!({"sequence":super::super::emotion::sequence()})),
        Ok(())
    );
}

fn assert_results(case: &Case, timing: &Timing, negative: bool, emotion: bool) {
    assert_eq!(case.released, ["bridge", "provider", "spoof", "relay"]);
    assert_eq!(timing.captured, 3200);
    assert_eq!(timing.played, 9600);
    let expected = if emotion && !negative {
        super::super::emotion::REPLY
    } else {
        text(negative)
    };
    assert_eq!(case.transcript.as_deref(), Some(expected));
    stages(&case.trace);
    first_audio(&case.trace);
    super::super::tests::assert_recorded_comparison(
        &case
            .trace
            .records
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m6_canonical_http500_never_animates_and_retains_honest_fallback() {
    exercise_mode(true, true).await;
}
