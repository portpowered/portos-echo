//! Controlled Wyoming peer and private consumption barriers; no bridge behavior is replaced.
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Phase {
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
    while wait_for_wake(&mut stream, requested.recv(), &mut observer).await {
        send(&mut stream, "run-pipeline", wake["header"]["data"].clone(), vec![]).await;
        stream = run_playback(stream, earcon).await;
        completed += 1;
        if completed == 2 {
            checkpoint(&mut observer, Phase::PlaybackEnded).await;
        }
    }
    // Socket EOF does not release this test-owned listener. The controller
    // explicitly drops its wake sender after collecting the bridge.
    checkpoint(&mut observer, Phase::Eof).await;
    assert!(requested.recv().await.is_none());
    drop(listener);
    completed
}

async fn run_playback(mut stream: tokio::net::TcpStream, earcon: bool) -> tokio::net::TcpStream {
    let format = json!({"rate":16000,"width":2,"channels":1});
    super::super::tests::send(&mut stream, "audio-start", format.clone(), vec![]).await;
    if earcon {
        super::super::wake::acknowledge(&mut stream).await;
    }
    let (stream, frames) = super::super::tests::continuous_playback(stream, format).await;
    assert_eq!(frames, 0);
    stream
}

// A synthetic satellite must keep answering the real bridge's idle probes
// while the test controls wake admission. Peek is cancellation safe: a wake
// cannot discard a partially read frame that playback still needs to consume.
pub(super) async fn wait_for_wake(
    stream: &mut tokio::net::TcpStream,
    requested: impl std::future::Future<Output = Option<()>>,
    observer: &mut Option<Observer>,
) -> bool {
    use super::super::tests::{read, send};
    tokio::pin!(requested);
    loop {
        let mut byte = [0];
        tokio::select! {
            requested = &mut requested => return requested.is_some(),
            ready = stream.peek(&mut byte) => if ready.unwrap() == 0 { return false; },
        }
        let (row, payload) = read(stream).await;
        assert_eq!(payload, [] as [u8; 0]);
        match row["type"].as_str().unwrap() {
            "describe" => send(stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "error" => assert_eq!(row["data"]["code"], "echo-kill-switch"),
            other => assert_eq!(other, "expected idle probe or kill refusal"),
        }
        if let Some(observer) = observer {
            observer
                .events
                .send(Phase::Control(row["type"].as_str().unwrap().into()))
                .unwrap();
        }
    }
}
