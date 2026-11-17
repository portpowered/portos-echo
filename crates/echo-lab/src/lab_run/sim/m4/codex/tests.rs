//! T3 component: same canonical runner, real pin/media/MCP, controlled Wyoming.
//! The complete milestone still requires the native hassmic-host scenario.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

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
    let options = options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (wake, awakened) = oneshot::channel();
    let satellite = tokio::spawn(satellite(listener, awakened));
    let mut case = Case::default();
    case.connect(&options, negative, address).await.unwrap();
    let path = case.root.as_ref().unwrap().0.clone();
    let mut screen = Screen::connect(&path).await.unwrap();
    screen.expect("ready").await.unwrap();
    wake.send(()).unwrap();
    let (connected, ready) = oneshot::channel();
    let spoof = case.spoof.as_ref().unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::try_join!(
            settle(
                case.bridge.as_mut().unwrap(),
                case.provider.as_mut().unwrap(),
                case.relay.as_ref().unwrap(),
                ready
            ),
            presentation(&mut screen, spoof, negative, connected)
        )
    })
    .await;
    let timing = case.relay.as_ref().unwrap().timing();
    let closed = case.close().await;
    assert_eq!(closed, Ok(()), "work: {observed:?}");
    assert_eq!(
        observed.unwrap().unwrap().0,
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_eq!(satellite.await.unwrap(), 9600);
    assert_eq!(timing.captured, 3200);
    assert_eq!(timing.played, 9600);
    assert_eq!(case.transcript.as_deref(), Some(text(negative)));
    stages(&case.trace);
    assert!(!path.exists());
}

async fn satellite(listener: TcpListener, wake: oneshot::Receiver<()>) -> usize {
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
    assert!(pcm.iter().all(|byte| *byte == 0));
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

fn options() -> Options {
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
    negative: bool,
    connected: oneshot::Sender<()>,
) -> Result<(), String> {
    screen.expect("listening").await?;
    screen.expect("connected").await?;
    if negative {
        spoof.set_fault(Fault::Http500).unwrap();
    }
    connected.send(()).unwrap();
    screen.expect("speaking").await?;
    screen.expect("ready").await?;
    Ok(())
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
