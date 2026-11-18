//! Controlled Wyoming peer and private consumption barriers; no bridge behavior is replaced.
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Phase {
    ReplyStopped,
    CutAcknowledged,
    PlaybackEnded,
    Control(String),
    Eof,
}

pub(super) struct Observer {
    pub events: mpsc::UnboundedSender<Phase>,
    pub resume: mpsc::Receiver<()>,
}

async fn checkpoint(observer: &mut Option<Observer>, phase: Phase) {
    if let Some(observer) = observer {
        observer.events.send(phase).unwrap();
        observer.resume.recv().await.unwrap();
    }
}

pub(super) async fn satellite(listener: Arc<TcpListener>, requested: mpsc::Receiver<()>, earcon: bool) -> usize {
    observed_satellite(listener, requested, earcon, None).await
}

pub(super) async fn observed_satellite(
    listener: Arc<TcpListener>,
    mut requested: mpsc::Receiver<()>,
    earcon: bool,
    mut observer: Option<Observer>,
) -> usize {
    use super::super::tests::{read, send};
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
    let mut completed = 0;
    while wait_for_wake(&mut stream, requested.recv(), &mut observer, completed > 0).await {
        send(&mut stream, "run-pipeline", wake["header"]["data"].clone(), vec![]).await;
        stream = run_playback(stream, earcon, &mut observer, completed == 1).await;
        completed += 1;
        if completed == 2 {
            notify(&observer, Phase::PlaybackEnded);
        }
    }
    // Socket EOF does not release this test-owned listener. The controller
    // explicitly drops its wake sender after collecting the bridge.
    checkpoint(&mut observer, Phase::Eof).await;
    assert!(requested.recv().await.is_none());
    drop(listener);
    completed
}

async fn run_playback(
    mut stream: tokio::net::TcpStream,
    earcon: bool,
    observer: &mut Option<Observer>,
    second: bool,
) -> tokio::net::TcpStream {
    use super::super::tests::{read, send};
    let format = json!({"rate":16000,"width":2,"channels":1});
    send(&mut stream, "audio-start", format.clone(), vec![]).await;
    if earcon {
        super::super::wake::acknowledge(&mut stream).await;
    }
    let (mut input, mut output) = stream.into_split();
    let mut cadence = tokio::time::interval(std::time::Duration::from_millis(20));
    let mut replying = false;
    let mut cut = false;
    loop {
        let (row, bytes) = capture_frame(&mut input, &mut output, &mut cadence, &format).await;
        match row["type"].as_str().unwrap() {
            "describe" => send(&mut output, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "audio-start" => replying = true,
            "audio-chunk" => panic!("capture-only lifecycle must receive zero reply PCM"),
            "hassmic-end" if cut => {
                control(&row, &bytes, &mut output, observer).await;
                send(&mut output, "audio-stop", json!({}), vec![]).await;
                break;
            }
            "hassmic-flush" | "hassmic-end" => {
                if end_capture(row, bytes, &mut output, &mut replying).await {
                    break;
                }
                cut = true;
                if second {
                    checkpoint(observer, Phase::CutAcknowledged).await;
                }
            }
            "audio-stop" => {
                if second {
                    checkpoint(observer, Phase::ReplyStopped).await;
                    if observer.is_some() {
                        continue;
                    }
                }
                send(&mut output, "audio-stop", json!({}), vec![]).await;
                send(&mut output, "played", json!({}), vec![]).await;
                let (end, payload) = read(&mut input).await;
                assert!(valid_end(&end, &payload));
                break;
            }
            other => assert_eq!(other, "expected reply end"),
        }
    }
    input.reunite(output).unwrap()
}

async fn end_capture(
    row: Value,
    bytes: Vec<u8>,
    output: &mut tokio::net::tcp::OwnedWriteHalf,
    replying: &mut bool,
) -> bool {
    use super::super::tests::send;
    let ended = row["type"] == "hassmic-end";
    let kind = if ended { "hassmic-end" } else { "hassmic-flush" };
    assert_eq!(row, json!({"type":kind,"data":null,"data_length":0,"payload_length":0}));
    assert!(bytes.is_empty());
    if ended {
        send(output, "audio-stop", json!({}), vec![]).await;
    }
    if *replying {
        send(output, "played", json!({}), vec![]).await;
        *replying = false;
    }
    ended
}

fn valid_end(row: &Value, bytes: &[u8]) -> bool {
    bytes.is_empty()
        && *row
            == json!({"type":"hassmic-end", "data":null,
        "data_length":0,"payload_length":0})
}

// A synthetic satellite must keep answering the real bridge's idle probes
// while the test controls wake admission. Peek is cancellation safe: a wake
// cannot discard a partially read frame that playback still needs to consume.
pub(super) async fn wait_for_wake(
    stream: &mut tokio::net::TcpStream,
    requested: impl std::future::Future<Output = Option<()>>,
    observer: &mut Option<Observer>,
    _drain_on_collect: bool,
) -> bool {
    use super::super::tests::read;
    tokio::pin!(requested);
    loop {
        let mut byte = [0];
        tokio::select! {
            requested = &mut requested => return requested.is_some(),
            ready = stream.peek(&mut byte) => if ready.unwrap() == 0 { return false; },
        }
        let (row, payload) = read(stream).await;
        control(&row, &payload, stream, observer).await;
    }
}

// End is a command, never a successful reply receipt. Reject external data,
// payloads and noncanonical headers rather than ignoring arbitrary input.
pub(super) fn valid_control(row: &Value, payload: &[u8]) -> bool {
    if !payload.is_empty() || row["payload_length"] != 0 || row["data_length"] != 0 {
        return false;
    }
    match row["type"].as_str() {
        Some("hassmic-end") => valid_end(row, payload),
        Some("error") => row["data"]["code"] == "echo-kill-switch" && row["data"]["text"].is_string(),
        Some("describe") => true,
        _ => false,
    }
}

fn notify(observer: &Option<Observer>, phase: Phase) {
    if let Some(observer) = observer {
        observer.events.send(phase).unwrap();
    }
}

async fn control(
    row: &Value,
    payload: &[u8],
    output: &mut (impl tokio::io::AsyncWrite + Unpin),
    observer: &Option<Observer>,
) {
    assert!(payload.is_empty());
    match row["type"].as_str().unwrap() {
        "describe" => {
            super::super::tests::send(output, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await
        }
        "error" => assert_eq!(row["data"]["code"], "echo-kill-switch"),
        other => assert_eq!(other, "expected idle probe or kill refusal"),
    }
    notify(observer, Phase::Control(row["type"].as_str().unwrap().into()));
}

// Keep the same full-frame future alive while capture ticks; cancellation can
// never discard a partly consumed header. Buffered controls win over a tick.
async fn capture_frame(
    input: &mut tokio::net::tcp::OwnedReadHalf,
    output: &mut tokio::net::tcp::OwnedWriteHalf,
    cadence: &mut tokio::time::Interval,
    format: &Value,
) -> (Value, Vec<u8>) {
    use super::super::tests::{read, send};
    let next = read(input);
    tokio::pin!(next);
    loop {
        tokio::select! {
            biased;
            row = &mut next => return row,
            _ = cadence.tick() => send(output, "audio-chunk", format.clone(), vec![0;640]).await,
        }
    }
}
