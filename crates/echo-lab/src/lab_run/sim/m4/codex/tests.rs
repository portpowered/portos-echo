//! T3 component: same canonical runner, real pin/media/MCP, controlled Wyoming.
//! The complete milestone still requires the native hassmic-host scenario.
use super::*;
#[path = "phase.rs"]
mod phase;
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
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(false)))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m4_canonical_provider_reports_mcp_failure_with_one_native_append() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(true)))
        .await
        .unwrap();
}

async fn exercise(negative: bool) {
    exercise_mode(negative, false).await;
}

async fn exercise_mode(negative: bool, emotion: bool) {
    let measurement = phase::Measurement::new(negative, emotion);
    let options = options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (wake, awakened) = oneshot::channel();
    let satellite = tokio::spawn(satellite(listener, awakened, measurement));
    let mut case = Case {
        emotion,
        ..Case::default()
    };
    case.connect(&options, negative, address).await.unwrap();
    measurement.mark("bridge ready");
    let path = case.root.as_ref().unwrap().0.clone();
    let mut screen = Screen::connect(&path).await.unwrap();
    screen.expect("ready").await.unwrap();
    wake.send(()).unwrap();
    let (connected, ready) = oneshot::channel();
    let spoof = case.spoof.as_ref().unwrap();
    let replies = case.provider.as_ref().unwrap().commands.clone();
    let observed = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::try_join!(
            settle(
                case.bridge.as_mut().unwrap(),
                case.provider.as_mut().unwrap(),
                case.relay.as_ref().unwrap(),
                ready,
                &mut case.clock
            ),
            presentation(&mut screen, spoof, (negative, emotion), connected, replies, measurement)
        )
    })
    .await;
    let timing = case.relay.as_ref().unwrap().timing();
    let closed = case.close().await;
    measurement.mark("receipts collected");
    assert_eq!(closed, Ok(()), "work: {observed:?}");
    assert_eq!(
        observed.as_ref().unwrap().as_ref().unwrap().0,
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_results(&case, &timing, negative, emotion);
    assert_eq!(satellite.await.unwrap(), 9600);
    assert_presentation(&observed.as_ref().unwrap().as_ref().unwrap().1, negative, emotion);
    assert!(!path.exists());
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
                json!({"state":"speaking"}),
                json!({"state":"ready","message":"Echo Live ready"})
            ]
        );
    }
}

async fn satellite(listener: TcpListener, wake: oneshot::Receiver<()>, measurement: phase::Measurement) -> usize {
    let mut socket = listener.accept().await.unwrap().0;
    assert_eq!(read(&mut socket).await.0["type"], "run-satellite");
    assert_eq!(read(&mut socket).await.0["type"], "describe");
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
    let count = playback(&mut socket).await;
    measurement.mark("media done");
    assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
    count
}
async fn playback(socket: &mut TcpStream) -> usize {
    let mut pcm = vec![];
    loop {
        let (header, bytes) = read(socket).await;
        match header["type"].as_str().unwrap() {
            "describe" => send(socket, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "audio-start" => assert_eq!(header["data"], json!({"rate":24000,"width":2,"channels":1})),
            "audio-chunk" => {
                assert_eq!(bytes.len(), 960);
                pcm.extend(bytes);
            }
            "audio-stop" => {
                send(socket, "played", json!({}), vec![]).await;
                break;
            }
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
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
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
    measurement: phase::Measurement,
) -> Result<Vec<Value>, String> {
    let (negative, emotion) = mode;
    let mut states = vec![];
    observe(screen, &mut states, "listening").await?;
    observe(screen, &mut states, "connected").await?;
    measurement.mark("provider/session ready");
    // Catalog discovery precedes session startup. This is an observation upper
    // bound at connected, not an invented tools/list arrival timestamp.
    if spoof
        .snapshot()
        .map_err(|_| "m4: missing MCP timing observation")?
        .requests
        .iter()
        .any(|request| request.method == "tools/list")
    {
        measurement.mark("tools listed (observed by connected)");
    }
    if negative {
        spoof.set_fault(Fault::Http500).unwrap();
    }
    connected.send(()).unwrap();
    if emotion {
        if !negative {
            observe(screen, &mut states, "emotion").await?;
            observe(screen, &mut states, "connected").await?;
        }
        replies.send(Control::Reply).await.unwrap();
    }
    observe(screen, &mut states, "speaking").await?;
    observe(screen, &mut states, "ready").await?;
    Ok(states)
}
fn stages(trace: &Trace) {
    let stages: Vec<_> = trace
        .records
        .iter()
        .filter(|r| {
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
        super::super::model::FALLBACK
    } else {
        super::super::model::SUCCESS
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m6_canonical_provider_preserves_exact_animation_transcript_and_pcm() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise_mode(false, true)))
        .await
        .unwrap();
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
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise_mode(true, true)))
        .await
        .unwrap();
}
