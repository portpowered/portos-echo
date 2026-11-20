use super::*;
use std::io::Write;

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
        let host = Host::new(&settings, std::sync::Arc::new(|| 0)).unwrap();
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
        (
            false,
            200,
            0,
            &fixture.screen,
            &mut fixture.host,
            &kill,
            Instant::now() + Duration::from_secs(600),
        ),
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
            (
                false,
                200,
                0,
                &fixture.screen,
                &mut fixture.host,
                &kill,
                Instant::now() + Duration::from_secs(600),
            ),
        ));
        pending(journey.as_mut()).await;
        assert_eq!(frames(&observer), vec![vec![0; 960]; usize::from(count)]);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        assert!(!trace(&path).iter().any(|r| r["event"] == "reply.played"));
        observer.finish(Ok(()));
        assert_eq!(journey.await.unwrap(), (0, true));
        assert_eq!(trace(&path).iter().filter(|r| r["outcome"] == "played").count(), 1);
        let snapshots: Vec<_> = trace(&path)
            .into_iter()
            .filter(|r| r["event"] == "mic.frames")
            .collect();
        assert_eq!(
            snapshots,
            vec![row("mic.frames", 0, json!({"phase":"completed","count":0}))]
        );
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
            (
                false,
                200,
                0,
                &fixture.screen,
                &mut fixture.host,
                &kill,
                Instant::now() + Duration::from_secs(600),
            ),
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

#[tokio::test(start_paused = true)]
async fn bridge_link_reused_call_isolates_different_and_empty_run_totals() {
    let mut fixture = Fixture::new().await;
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    let mut live = Live::default();
    let mut totals = Vec::new();
    let path = fixture.directory.path().join("shared-trace");
    let mut writer = Trace::open(&path, "api_key").unwrap();
    for (index, (samples, cancel)) in [(1024, true), (1024, false), (0, false), (2048, false), (320, false)]
        .into_iter()
        .enumerate()
    {
        let id = format!("run-{index}");

        observer.wake("Computer", capture(samples));
        let run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
        live.audio = reply(1);
        live.events = if cancel {
            VecDeque::new()
        } else {
            [LiveEvent::Other(json!({"type":"response.audio.done"}))].into()
        };
        live.cancel_at = cancel.then(|| Instant::now() + Duration::from_millis(1));
        let base = live.sent.len() as u64;
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
            &mut writer,
            &id,
            (
                false,
                200,
                base,
                &fixture.screen,
                &mut fixture.host,
                &kill,
                Instant::now() + Duration::from_secs(600),
            ),
        ));
        pending(journey.as_mut()).await;
        assert!(
            !trace(&path)
                .iter()
                .any(|r| r["phase"] == "completed" && r["run_id"] == id)
        );
        if cancel {
            tokio::time::advance(Duration::from_millis(1)).await;
            pending(journey.as_mut()).await;
        }
        observer.finish(Ok(()));
        let (count, completed) = journey.await.unwrap();
        check_reused_run((&path, &id, samples, cancel), &live, base, count, completed);
        totals.push(count);
        writeln!(std::io::stdout(), "{id} accepted={count}").unwrap();
    }
    assert_eq!(totals, [2, 3, 0, 6, 1]);
    link.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn bridge_link_partial_send_failure_never_claims_completed() {
    let mut fixture = Fixture::new().await;
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    observer.wake(
        "Computer",
        vec![MicFrame {
            format: bridge_audio::format(16_000),
            payload: vec![0; 2048],
        }],
    );
    let run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
    let mut live = Live {
        reject_at: Some(1),
        ..Live::default()
    };
    let path = fixture.directory.path().join("trace");
    let mut writer = Trace::open(&path, "api_key").unwrap();
    let kill = Kill {
        settings: &fixture.settings,
        health: &fixture.health,
        screen: &fixture.screen,
    };
    assert_eq!(
        super::super::run(
            &link,
            &mut events,
            run,
            &mut live,
            &mut writer,
            "run-1",
            (
                true,
                200,
                0,
                &fixture.screen,
                &mut fixture.host,
                &kill,
                Instant::now() + Duration::from_secs(600)
            )
        )
        .await,
        Err(Error::Unavailable)
    );
    assert_eq!(live.sent.len(), 1);
    assert!(!trace(&path).iter().any(|r| r["phase"] == "completed"));
    link.close().await.unwrap();
}

fn assert_run_snapshots(path: &std::path::Path, id: &str, samples: usize, count: u64, receipt: u64) {
    let rows: Vec<_> = trace(path).into_iter().filter(|r| r["run_id"] == id).collect();
    let snapshots: Vec<_> = rows.iter().filter(|r| r["event"] == "mic.frames").collect();
    assert_eq!(snapshots.len(), if samples == 0 { 1 } else { 2 });
    assert_eq!(snapshots.last().unwrap()["phase"], "completed");
    assert_eq!(snapshots.last().unwrap()["count"].as_u64(), Some(receipt));
    assert!(snapshots.iter().all(|r| r["run_id"] == id));
    assert_eq!(
        rows.iter().find(|r| r["event"] == "mic.completed").unwrap()["count"],
        count
    );
    if samples > 0 {
        assert_eq!(snapshots[0]["phase"], "first");
        assert!(
            rows.iter().position(|r| r["phase"] == "first") < rows.iter().position(|r| r["event"] == "reply.started")
        );
    }
}

fn check_reused_run(case: (&std::path::Path, &str, usize, bool), live: &Live, base: u64, count: u64, completed: bool) {
    let (path, id, samples, cancel) = case;
    assert_eq!(completed, !cancel);
    let receipt = live.sent.len() as u64 - base;
    assert_eq!(count, receipt);
    if cancel {
        let rows = trace(path);
        let snapshots: Vec<_> = rows
            .iter()
            .filter(|r| r["event"] == "mic.frames" && r["run_id"] == id)
            .collect();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["phase"], "first");
        assert_eq!(snapshots[0]["count"], receipt);
    } else {
        assert_run_snapshots(path, id, samples, count, receipt);
    }
    if samples > 0 {
        assert_eq!(
            live.sent[usize::try_from(base).unwrap()].sequence,
            u16::try_from(base).unwrap()
        );
    }
}

fn capture(samples: usize) -> Vec<MicFrame> {
    if samples == 0 {
        vec![]
    } else {
        vec![MicFrame {
            format: bridge_audio::format(16_000),
            payload: vec![0; samples * 2],
        }]
    }
}

#[tokio::test(start_paused = true)]
async fn bridge_link_continuation_requires_new_input_and_preserves_admitted_totals() {
    let mut fixture = Fixture::new().await;
    let observer = Observer::default();
    let link = FakeConnector::new(observer).connect().await.unwrap();
    let mut writer = Trace::open(&fixture.directory.path().join("trace"), "api_key").unwrap();
    let kill = Kill {
        settings: &fixture.settings,
        health: &fixture.health,
        screen: &fixture.screen,
    };
    let mut context = PlaybackContext {
        client: &link,
        trace: &mut writer,
        id: "run-1",
        lead_ms: 200,
        screen: &fixture.screen,
        speaking: true,
        microphone: true,
        microphone_at_reply: Some(44),
        played: true,
        cancellation: Cancellation::Response,
        cut: None,
        wait: bridge_timing::Wait::new(Instant::now()),
        delegation: &mut fixture.host,
        kill: &kill,
    };
    let mut source = Source::new().unwrap();
    source.sent = 44;
    source.chunks = 17;
    source.call_packets_base = 10;
    source.cancelled = true;
    assert!(!context.continuing(&source));
    source.sent = 49;
    assert!(context.continuing(&source));
    context.cancellation = Cancellation::Shutdown;
    assert!(!context.continuing(&source));
    context.cancellation = Cancellation::Response;
    context.microphone_at_reply = None;
    assert!(!context.continuing(&source));
    context.microphone_at_reply = Some(44);
    source.finished = true;
    assert!(context.continuing(&source));
    source.finished = false;
    source.audio_done = true;
    source.resume_reply().unwrap();
    assert_eq!((source.sent, source.chunks, source.call_packets_base), (49, 17, 10));
    assert!(!source.finished);
    assert!(!source.cancelled);
    assert!(!source.audio_done);
    assert!(!context.continuing(&source));
}

fn marker(sample: i16) -> MicFrame {
    MicFrame {
        format: bridge_audio::format(16_000),
        payload: sample.to_le_bytes().repeat(1024),
    }
}

#[tokio::test(start_paused = true)]
async fn bridge_link_interrupting_tail_is_forwarded_while_awaiting_old_played_and_after_resume() {
    let mut fixture = Fixture::new().await;
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    observer.wake("Computer", vec![marker(1200)]);
    let mut run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
    let mut writer = Trace::open(&fixture.directory.path().join("trace"), "api_key").unwrap();
    let kill = Kill {
        settings: &fixture.settings,
        health: &fixture.health,
        screen: &fixture.screen,
    };
    let mut context = PlaybackContext {
        client: &link,
        trace: &mut writer,
        id: "run-1",
        lead_ms: 200,
        screen: &fixture.screen,
        speaking: true,
        microphone: true,
        microphone_at_reply: Some(0),
        played: false,
        cancellation: Cancellation::Response,
        cut: None,
        wait: bridge_timing::Wait::new(Instant::now()),
        delegation: &mut fixture.host,
        kill: &kill,
    };
    let mut source = Source::new().unwrap();
    source.cancelled = true;
    let mut live = Live::default();
    let mut wait = Box::pin(context.await_played(&mut events, &mut run, &mut source, &mut live));
    pending(wait.as_mut()).await;
    observer.finish(Ok(()));
    wait.await.unwrap();
    assert_eq!(source.mic.as_ref().unwrap().input_samples, 1024);
    assert!(context.continuing(&source));
    source.resume_reply().unwrap();
    assert!(!source.finished);
    context
        .capture(&mut source, Some(marker(-2400)), &mut live, false)
        .await
        .unwrap();
    context.capture(&mut source, None, &mut live, false).await.unwrap();
    assert_eq!(source.mic.as_ref().unwrap().input_samples, 2048);
    let mut expected = bridge_audio::Microphone::new(bridge_audio::format(16_000)).unwrap();
    let mut packets = expected.process(&marker(1200).format, &marker(1200).payload).unwrap();
    packets.extend(expected.process(&marker(-2400).format, &marker(-2400).payload).unwrap());
    packets.extend(expected.finish().unwrap());
    assert_eq!(live.sent.len(), packets.len());
    for (actual, expected) in live.sent.iter().zip(packets) {
        assert_eq!(actual.payload, expected.payload);
        assert_eq!(actual.sequence, expected.sequence);
    }
    assert_eq!(source.sent, 6);
    assert!(source.finished);
    link.close().await.unwrap();
}
