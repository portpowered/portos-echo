use super::*;

struct Fixture {
    settings: bridge_input::Settings,
    screen: Screen,
    health: Health,
    host: Host,
    directory: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let settings = settings();
        let host = Host::new(&settings, std::sync::Arc::new(|| 0));
        Self {
            settings,
            screen: Screen::bind(None).await.unwrap(),
            health: Health::offline(),
            host,
            directory: tempfile::tempdir().unwrap(),
        }
    }
}
fn reply(count: u16) -> VecDeque<AudioFrame> {
    let mut encoder = echo_audio::OpusEncoder::new().unwrap();
    (0..count)
        .map(|sequence| {
            let mut bytes = [0; 1275];
            let size = encoder.encode(&[0; 960], &mut bytes).unwrap();
            AudioFrame {
                sequence,
                timestamp: u32::from(sequence) * 960,
                payload: bytes[..size].to_vec(),
            }
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn bridge_link_cancel_discards_backlog_once_excluding_evictions_and_tail() {
    let mut fixture = Fixture::new().await;
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    observer.wake("Computer", vec![]);
    let run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
    let mut live = Live {
        audio: reply(40),
        cancel_at: Some(Instant::now() + Duration::from_millis(1)),
        ..Live::default()
    };
    let path = fixture.directory.path().join("trace");
    let mut trace_writer = Trace::open(&path, "api_key").unwrap();
    let kill = Kill {
        settings: &fixture.settings,
        health: &fixture.health,
        screen: &fixture.screen,
    };
    let mut journey = Box::pin(super::super::run(
        &link,
        &mut events,
        run,
        &mut live,
        &mut trace_writer,
        "run-1",
        (false, 200, 0, &fixture.screen, &mut fixture.host, &kill),
    ));
    pending(journey.as_mut()).await;
    assert_eq!(frames(&observer).len(), 10);
    tokio::time::advance(Duration::from_millis(1)).await;
    pending(journey.as_mut()).await;
    assert_eq!(frames(&observer).len(), 10);
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Cut(_)))
            .count(),
        1
    );
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    assert!(!trace(&path).iter().any(|r| r["event"] == "reply.played"));
    observer.finish(Ok(()));
    assert_eq!(journey.await.unwrap(), (0, false));
    assert_cut_trace(&path);
    link.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn bridge_link_empty_and_one_frame_replies_require_actual_acknowledgment() {
    for count in [0, 1] {
        let mut fixture = Fixture::new().await;
        let observer = Observer::default();
        let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = link.events().unwrap();
        observer.wake("Computer", vec![]);
        let run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
        let mut live = Live {
            audio: reply(count),
            events: [LiveEvent::Other(json!({"type":"response.audio.done"}))].into(),
            ..Live::default()
        };
        let path = fixture.directory.path().join("trace");
        let mut trace_writer = Trace::open(&path, "api_key").unwrap();
        let kill = Kill {
            settings: &fixture.settings,
            health: &fixture.health,
            screen: &fixture.screen,
        };
        let mut journey = Box::pin(super::super::run(
            &link,
            &mut events,
            run,
            &mut live,
            &mut trace_writer,
            "run-1",
            (false, 200, 0, &fixture.screen, &mut fixture.host, &kill),
        ));
        pending(journey.as_mut()).await;
        assert_eq!(frames(&observer), vec![vec![0; 960]; usize::from(count)]);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        assert!(!trace(&path).iter().any(|r| r["event"] == "reply.played"));
        observer.finish(Ok(()));
        assert_eq!(journey.await.unwrap(), (0, true));
        assert_eq!(trace(&path).iter().filter(|r| r["outcome"] == "played").count(), 1);
        link.close().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn bridge_link_transport_protocol_and_timeout_never_claim_played() {
    for failure in [
        Some(echo_link::Error::Transport),
        Some(echo_link::Error::Protocol),
        Some(echo_link::Error::Timeout),
        None,
    ] {
        let mut fixture = Fixture::new().await;
        let observer = Observer::default();
        let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = link.events().unwrap();
        observer.wake("Computer", vec![]);
        let run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
        let mut live = Live {
            events: [LiveEvent::Other(json!({"type":"response.audio.done"}))].into(),
            ..Live::default()
        };
        let path = fixture.directory.path().join("trace");
        let mut trace_writer = Trace::open(&path, "api_key").unwrap();
        let kill = Kill {
            settings: &fixture.settings,
            health: &fixture.health,
            screen: &fixture.screen,
        };
        let mut journey = Box::pin(super::super::run(
            &link,
            &mut events,
            run,
            &mut live,
            &mut trace_writer,
            "run-1",
            (false, 200, 0, &fixture.screen, &mut fixture.host, &kill),
        ));
        pending(journey.as_mut()).await;
        if let Some(error) = failure {
            observer.finish(Err(error));
        } else {
            tokio::time::advance(Duration::from_secs(5)).await;
        }
        let expected = if matches!(failure, Some(echo_link::Error::Timeout) | None) {
            Error::Deadline
        } else {
            Error::Wyoming
        };
        assert_eq!(journey.await, Err(expected));
        assert_eq!(trace(&path), Vec::<serde_json::Value>::new());
        link.close().await.unwrap();
        assert_eq!(observer.commands().last(), Some(&Command::Close));
    }
}

fn assert_cut_trace(path: &std::path::Path) {
    assert_eq!(
        trace(path),
        vec![
            row("reply.started", 0, json!({})),
            row(
                "reply.cut",
                1,
                json!({"bridge_dropped":29,"transport_dropped":0,
            "dropped_frames":29,"bridge_evicted":0,"reply_frames":39})
            ),
            row("reply.played", 1, json!({"outcome":"cut"})),
            row("wyoming.mic_frames", 1, json!({"count":0})),
            row(
                "mic.completed",
                1,
                json!({"count":0,"received_chunks":0,"dropped_chunks":0,
            "input_samples":0,"tail_samples":0})
            ),
            row("wyoming.run_ended", 1, json!({})),
        ]
    );
}
