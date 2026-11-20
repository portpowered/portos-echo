use super::*;
use crate::bridge_mcp::bridge_presentation::Presentation;
use echo_link::LinkEvent;
use echo_link::{
    EchoLink, LinkConnector, LinkEvents,
    fake::{Command, FakeConnector, Observer},
};
use tokio::sync::mpsc;

#[tokio::test(start_paused = true)]
async fn fast_connection_keeps_listening_until_earcon_receipt_and_reuse_never_republishes_connected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(16);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let work = super::super::earcon::scope(&client, true, async {
        for index in 1..=2 {
            project_chime(&mut view, &mut receive, &mut events, index == 1).await;
            complete(&mut view, &mut receive);
        }
        Ok(())
    });
    let remote = async {
        for index in 1..=2 {
            while observer
                .commands()
                .iter()
                .filter(|c| matches!(c, Command::Cut(_)))
                .count()
                < index
            {
                tokio::task::yield_now().await;
                tokio::time::advance(std::time::Duration::from_millis(20)).await;
            }
            observer.finish(Ok(()));
        }
    };
    let (result, ()) = tokio::join!(work, remote);
    assert_eq!(result, Ok(()));
    let chimes: Vec<_> = rows(&path)
        .into_iter()
        .filter(|r| r["event"] == "wake.earcon_played")
        .collect();
    assert_eq!(chimes.len(), 2);
    for (index, chime) in chimes.iter().enumerate() {
        assert_eq!(chime["run_id"], format!("run-{}", index + 1));
        assert_eq!(chime["duration_ms"], 160);
    }
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Frame(_)))
            .count(),
        16
    );
}

async fn project_chime(
    view: &mut View<'_>,
    receive: &mut mpsc::Receiver<UiState>,
    events: &mut impl LinkEvents,
    fresh: bool,
) {
    use super::super::earcon;
    if fresh {
        view.observe(ServeNotice::Listening).unwrap();
    } else {
        view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
    }
    assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
    earcon::start().unwrap();
    if fresh {
        let origin = tokio::time::Instant::now();
        earcon::negotiating(events, async { Ok(()) }, &mut |_| unreachable!())
            .await
            .unwrap();
        assert_eq!(tokio::time::Instant::now(), origin);
        view.observe(ServeNotice::Session(SessionNotice::Connected)).unwrap();
        assert_eq!(view.health.state.borrow().live, "connected");
    }
    assert!(earcon::active());
    assert!(receive.try_recv().is_err());
    let duration = chime_receipt(events, receive).await;
    assert_eq!(duration, std::time::Duration::from_millis(160));
    view.observe(ServeNotice::Session(SessionNotice::Run(Notice::EarconPlayed(duration))))
        .unwrap();
    if fresh {
        assert_eq!(receive.try_recv().unwrap(), UiState::Connected);
    }
    assert!(receive.try_recv().is_err());
}

async fn chime_receipt(events: &mut impl LinkEvents, receive: &mut mpsc::Receiver<UiState>) -> std::time::Duration {
    use super::super::earcon;
    loop {
        tokio::select! {
            biased;
            duration = earcon::completed() => return duration,
            event = events.recv() => {
                assert!(earcon::receipt(&event.unwrap().unwrap()));
                assert!(receive.try_recv().is_err());
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn failed_negotiation_clears_pending_connected_without_earcon_or_reply_success() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(16);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let work = super::super::earcon::scope(&client, true, async {
        view.observe(ServeNotice::Listening)?;
        super::super::earcon::start()?;
        let result = super::super::earcon::negotiating(
            &mut events,
            async { Err::<(), _>(Error::Unavailable) },
            &mut |_| unreachable!(),
        )
        .await;
        assert_eq!(result, Err(Error::Unavailable));
        view.closed("failed")?;
        assert!(!view.pending_connected);
        assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
        assert!(receive.try_recv().is_err());
        Ok(())
    });
    let remote = async {
        while !observer.commands().iter().any(|c| matches!(c, Command::Cut(_))) {
            tokio::task::yield_now().await;
        }
        observer.finish(Ok(()));
    };
    let (result, ()) = tokio::join!(work, remote);
    assert_eq!(result, Ok(()));
    assert_eq!(health.state.borrow().live, "idle");
    for event in ["wake.earcon_played", "reply.played", "bridge.run_completed"] {
        assert_eq!(rows(&path).iter().filter(|r| r["event"] == event).count(), 0);
    }
}

fn rows(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn killed_overlay_survives_mute_and_ready_and_clear_restores_idle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(16);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Listening).unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Connected)).unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
    assert_eq!(receive.try_recv().unwrap(), UiState::Connected);
    view.observe(ServeNotice::Kill(true)).unwrap();
    let killed = receive.try_recv().unwrap();
    assert_eq!(
        killed,
        UiState::Error {
            message: "Kill switch enabled. Run echo-bridge kill --clear with your config.".into(),
            relink: false,
        }
    );
    view.observe(ServeNotice::Ready).unwrap();
    assert_eq!(receive.try_recv().unwrap(), killed);
    assert!(health.state.borrow().kill_switch);
    assert_eq!(health.state.borrow().live, "error");
    view.observe(ServeNotice::Device(LinkEvent::Mute(true))).unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
    view.observe(ServeNotice::Device(LinkEvent::Mute(false))).unwrap();
    assert_eq!(receive.try_recv().unwrap(), killed);
    view.observe(ServeNotice::Kill(false)).unwrap();
    assert_eq!(
        receive.try_recv().unwrap(),
        UiState::Ready {
            message: "Echo Live ready".into()
        }
    );
    assert!(!health.state.borrow().kill_switch);
    assert_eq!(health.state.borrow().live, "idle");
    let rows = rows(&path);
    assert_eq!(rows.iter().filter(|row| row["event"] == "live.closed").count(), 1);
    assert_eq!(
        rows.iter().find(|row| row["event"] == "live.closed").unwrap()["reason"],
        "kill"
    );
    for event in ["kill.enabled", "kill.cleared"] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 1);
    }
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "bridge.run_completed").count(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn projection_distinguishes_negotiation_played_reuse_and_idle_stop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(16);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Ready).unwrap();
    assert_eq!(
        receive.try_recv().unwrap(),
        UiState::Ready {
            message: "Echo Live ready".into()
        }
    );
    assert_eq!(health.state.borrow().live, "idle");
    view.observe(ServeNotice::Listening).unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
    assert_eq!(health.state.borrow().live, "connecting");
    view.observe(ServeNotice::Session(SessionNotice::Connected)).unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Connected);
    assert_eq!(health.state.borrow().live, "connected");
    view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
    assert!(receive.try_recv().is_err());
    for run in 1..=2 {
        complete(&mut view, &mut receive);
        assert_eq!(health.state.borrow().live, "idle");
        assert_eq!(
            rows(&path)
                .iter()
                .filter(|row| row["event"] == "bridge.run_completed")
                .count(),
            run
        );
        if run == 1 {
            view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
            assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
            assert_eq!(health.state.borrow().live, "connected");
        }
    }
    view.observe(ServeNotice::Ready).unwrap();
    view.closed("shutdown").unwrap();
    completed_trace(&path);
}

fn completed_trace(path: &std::path::Path) {
    let rows = rows(path);
    assert_eq!(rows.iter().filter(|row| row["event"] == "live.closed").count(), 1);
    assert_eq!(
        rows.iter().find(|row| row["event"] == "live.closed").unwrap()["reason"],
        "idle"
    );
    for (index, row) in rows
        .iter()
        .filter(|row| row["event"] == "bridge.run_completed")
        .enumerate()
    {
        assert_eq!(row["run_id"], format!("run-{}", index + 1));
    }
    for played in rows.iter().filter(|row| row["event"] == "reply.played") {
        assert_eq!(played["outcome"], "played");
        assert_eq!(played["frames"], 8);
        assert_eq!(played["received_frames"], 9);
        assert_eq!(played["dropped_frames"], 1);
    }
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("private transcript fixture")
    );
}

pub(super) fn complete(view: &mut View<'_>, receive: &mut mpsc::Receiver<UiState>) {
    view.observe(ServeNotice::Session(SessionNotice::Run(Notice::ReplyStarted)))
        .unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Speaking);
    for notice in [
        Notice::Transcript {
            role: "assistant".into(),
            text: "private transcript fixture".into(),
            done: true,
        },
        Notice::ReplyPlayed,
    ] {
        view.observe(ServeNotice::Session(SessionNotice::Run(notice))).unwrap();
        assert!(receive.try_recv().is_err());
    }
    view.observe(ServeNotice::Session(SessionNotice::Run(Notice::ReplyPaused)))
        .unwrap();
    assert_eq!(
        receive.try_recv().unwrap(),
        UiState::Ready {
            message: "Echo Live ready".into()
        }
    );
    let stats = Stats {
        received_chunks: 7,
        microphone_frames: 3,
        reply_received: 9,
        reply_sent: 8,
        reply_dropped: 1,
        ..Stats::default()
    };
    view.observe(ServeNotice::Session(SessionNotice::MicrophoneCompleted(stats.clone())))
        .unwrap();
    view.observe(ServeNotice::Session(SessionNotice::ReplyCompleted(stats.clone())))
        .unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Completed(stats)))
        .unwrap();
    assert!(receive.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn reply_receipts_keep_run_and_tool_correlation_until_explicit_end() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let mut effect = trace.clone();
    let health = Health::offline();
    let screen = Screen::bind(None).await.unwrap();
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Listening).unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Connected)).unwrap();
    for frames in [3, 7] {
        let notice = ServeNotice::Session(SessionNotice::ReplyCompleted(Stats {
            reply_sent: frames,
            ..Stats::default()
        }));
        view.observe(notice).unwrap();
        effect.tool_attempt("test.echo", "1:t1:u1:c").unwrap();
        assert_eq!(health.state.borrow().live, "connected");
    }
    let before_end = rows(&path);
    assert_eq!(
        before_end.iter().filter(|r| r["event"] == "wyoming.run_ended").count(),
        0
    );
    assert_eq!(
        before_end
            .iter()
            .filter(|r| r["event"] == "bridge.run_completed")
            .count(),
        0
    );
    let replies: Vec<_> = before_end.iter().filter(|r| r["event"] == "reply.played").collect();
    assert_eq!(
        replies.iter().map(|r| r["frames"].clone()).collect::<Vec<_>>(),
        [json!(3), json!(7)]
    );
    for row in &before_end {
        if row["event"] == "reply.played" || row["event"] == "tools/call" {
            assert_eq!(row["run_id"], "run-1");
        }
    }
    view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default())))
        .unwrap();
    assert_eq!(
        rows(&path)
            .iter()
            .filter(|r| r["event"] == "bridge.run_completed")
            .count(),
        1
    );
    assert_eq!(
        view.observe(ServeNotice::Session(SessionNotice::ReplyCompleted(Stats::default()))),
        Err(Error::Unavailable)
    );
}

#[tokio::test(start_paused = true)]
async fn tool_receipts_follow_reused_wakes_and_never_keep_a_finished_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let mut effect = trace.clone();
    let health = Health::offline();
    let screen = Screen::bind(None).await.unwrap();
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Listening).unwrap();
    effect.tool_attempt("test.echo", "1:t1:u1:c").unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default())))
        .unwrap();
    effect.tool_attempt("test.echo", "1:t1:u1:d").unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Started)).unwrap();
    effect.tool_attempt("test.echo", "1:t1:v1:c").unwrap();
    view.observe(ServeNotice::Kill(true)).unwrap();
    effect.tool_attempt("test.echo", "1:t1:v1:d").unwrap();
    let runs: Vec<_> = rows(&path)
        .into_iter()
        .filter(|row| row["event"] == "tools/call")
        .map(|row| row["run_id"].clone())
        .collect();
    assert_eq!(runs, vec![json!("run-1"), json!(null), json!("run-2"), json!(null)]);
}

#[tokio::test(start_paused = true)]
async fn completed_state_queue_failure_never_claims_run_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(1);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Listening).unwrap();
    assert_eq!(
        view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default()))),
        Err(Error::Screen)
    );
    assert_eq!(receive.try_recv().unwrap(), UiState::Listening);
    assert_eq!(
        rows(&path)
            .iter()
            .filter(|row| row["event"] == "bridge.run_completed")
            .count(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn projection_privacy_relink_and_publication_failure_preserve_honest_health() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = Trace::open(&path, "chatgpt").unwrap();
    let health = Health::offline();
    let (send, mut receive) = mpsc::channel(1);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Device(LinkEvent::Mute(true))).unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
    view.observe(ServeNotice::Device(LinkEvent::MicrophoneDropped(4)))
        .unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Run(Notice::Device(
        LinkEvent::MicrophoneDropped(1),
    ))))
    .unwrap();
    view.observe(ServeNotice::Session(SessionNotice::Run(Notice::ReplyStarted)))
        .unwrap();
    assert!(receive.try_recv().is_err());
    view.failed(&Error::Authorization).await.unwrap();
    assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
    assert_eq!(health.state.borrow().live, "error");
    assert!(health.state.borrow().relink_required);
    view.observe(ServeNotice::Device(LinkEvent::Mute(false))).unwrap();
    assert_eq!(
        receive.try_recv().unwrap(),
        crate::bridge_runtime::bridge_screen::failure_state(true)
    );
    view.failed(&Error::Authorization).await.unwrap();
    assert_eq!(
        receive.try_recv().unwrap(),
        crate::bridge_runtime::bridge_screen::failure_state(true)
    );
    let windows: Vec<_> = rows(&path)
        .into_iter()
        .filter(|row| row["event"] == "mic.mute_window")
        .collect();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["received_frames"], 5);
    assert_eq!(windows[0]["forwarded_frames"], 0);
    screen.presentation.refresh().unwrap();
    assert_eq!(view.failed(&Error::Authorization).await, Err(Error::Screen));
    assert_eq!(
        rows(&path)
            .iter()
            .filter(|row| row["event"] == "auth.relink_required")
            .count(),
        2
    );
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("private transcript fixture")
    );
}
