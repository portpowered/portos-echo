//! T3: after readiness no failure ends the configured owner. Controlled child,
//! device and media ports; each journey ended the bridge before recovery existed.
use super::tools::{Media, config, ready, result};
use super::*;
use echo_bridge::bridge_voice::Configured;
use echo_link::{
    RunFailure,
    fake::{Command, FakeConnector, Observer},
};
use echo_media::PeerState;
use std::sync::{Arc, OnceLock};
use tokio::sync::{Notify, watch};

struct Outcome {
    rows: Vec<Value>,
    commands: Vec<Command>,
    launches: usize,
}

impl Outcome {
    fn count(&self, event: &str) -> usize {
        self.rows.iter().filter(|row| row["event"] == event).count()
    }

    fn sent(&self, command: &Command) -> usize {
        self.commands.iter().filter(|sent| *sent == command).count()
    }

    /// One device connection for the whole journey, and no terminal error.
    fn stayed_up(&self, retries: usize) {
        assert_eq!(self.count("bridge.error"), 0);
        assert_eq!(self.count("bridge.recovering"), retries);
        assert_eq!(self.count("bridge.ready"), retries + 1);
        assert_eq!(self.launches, retries + 1);
        assert_eq!(self.sent(&Command::Connect), 1);
        assert_eq!(self.sent(&Command::Capture), 1);
        assert_eq!(self.sent(&Command::Close), 1);
        assert_eq!(self.commands.last(), Some(&Command::Close));
    }
}

async fn traced(path: &Path, event: &str, count: usize) {
    while trace(path).iter().filter(|row| row["event"] == event).count() < count {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn sent(observer: &Observer, command: &Command, count: usize) {
    while observer.commands().iter().filter(|sent| *sent == command).count() < count {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn cuts(observer: &Observer) -> usize {
    let cut = |sent: &&Command| matches!(sent, Command::Cut(_));
    observer.commands().iter().filter(cut).count()
}

/// The device acknowledges the cut of the playback an ended run left open.
async fn cut(observer: &Observer, count: usize) {
    while cuts(observer) < count {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    observer.finish(Ok(()));
}

/// Run the configured owner until `journey` returns, then stop it cleanly.
/// The owner ending by itself at any point fails the journey.
async fn journey(mode: &str, journey: impl AsyncFnOnce(&Fixture, &Path, &Observer, &Notify)) -> Outcome {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), mode).unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut config = config(&fixture, spoof.url());
    config.voice.session_listen_seconds = 1;
    let path = fixture.directory.path().join("recovery-trace");
    let observer = Observer::default();
    let lost = Arc::new(Notify::new());
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let prepared = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || {
            Ok(Media {
                state: watch::channel(PeerState::New).0,
                closed: Arc::default(),
                setup: OnceLock::new(),
                lost: lost.clone(),
            })
        },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let stopped = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::pin!(work);
        tokio::select! {
            result = &mut work => panic!("the bridge ended by itself: {result:?}"),
            () = journey(&fixture, &path, &observer, &lost) => (),
        }
        // A stop during an open run cuts its playback and awaits the receipt.
        let before = cuts(&observer);
        stop.send(()).unwrap();
        let receipts = async {
            cut(&observer, before + 1).await;
            std::future::pending::<()>().await;
        };
        tokio::select! {
            result = &mut work => result,
            () = receipts => unreachable!(),
        }
    }))
    .await
    .unwrap();
    assert_eq!(stopped, Ok(()));
    super::host::released(&fixture).await;
    spoof.shutdown().await.unwrap();
    Outcome {
        rows: trace(&path),
        commands: observer.commands(),
        launches: std::fs::read_to_string(fixture.home.join("launches"))
            .unwrap()
            .lines()
            .count(),
    }
}

/// The model's delegated tool call returned: the call is connected and the
/// device run is open.
async fn answered(fixture: &Fixture) {
    result(fixture).await;
    std::fs::remove_file(fixture.home.join("tool-result")).unwrap();
}

// The second device incident: Live media failed during a run. That run ends on
// the device with the existing error, the link stays, the next wake is served.
#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_recovery_live_failure_during_a_run_ends_that_run_and_the_next_wake_is_served() {
    let outcome = journey("tool-echo", async |fixture, path, observer, lost| {
        let _ready = ready(fixture).await;
        observer.wake("first", vec![]);
        answered(fixture).await;
        lost.notify_one();
        cut(observer, 1).await;
        traced(path, "bridge.ready", 2).await;
        observer.wake("second", vec![]);
        answered(fixture).await;
    })
    .await;
    outcome.stayed_up(1);
    assert_eq!(outcome.count("run.started"), 2);
    assert_eq!(outcome.sent(&Command::FailRun(RunFailure::Unavailable)), 1);
    let retry = outcome
        .rows
        .iter()
        .find(|row| row["event"] == "bridge.recovering")
        .unwrap();
    assert_eq!(
        (&retry["run_id"], &retry["attempt"], &retry["delay_ms"]),
        (&json!("run-1"), &json!(1), &json!(1000))
    );
}

// The first device incident: the call kept warm between wakes lost its media
// while idle. No device run is open, so the device hears nothing about it.
#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_recovery_idle_call_media_failure_keeps_the_bridge_and_the_next_wake_starts_a_run() {
    let outcome = journey("tool-reply", async |fixture, path, observer, lost| {
        let _ready = ready(fixture).await;
        for run in 1..=2 {
            observer.wake("reply", vec![]);
            sent(observer, &Command::Stop, run).await;
            observer.finish(Ok(()));
            traced(path, "bridge.run_completed", run).await;
            if run == 1 {
                lost.notify_one();
                traced(path, "bridge.ready", 2).await;
            }
        }
    })
    .await;
    outcome.stayed_up(1);
    assert_eq!(outcome.count("run.started"), 2);
    assert_eq!(outcome.sent(&Command::FailRun(RunFailure::Unavailable)), 0);
    let closes: Vec<_> = outcome
        .rows
        .iter()
        .filter(|row| row["event"] == "live.closed")
        .map(|row| row["reason"].as_str().unwrap())
        .collect();
    assert_eq!(closes[0], "failed");
}

// The Codex app-server child dying between wakes is replaced, not fatal.
#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_recovery_child_exit_respawns_and_the_next_wake_is_served() {
    let outcome = journey("tool-echo", async |fixture, path, observer, _| {
        let _ready = ready(fixture).await;
        let child = std::fs::read_to_string(fixture.home.join("peer-pid")).unwrap();
        let killed = std::process::Command::new("kill")
            .args(["-9", child.trim()])
            .status()
            .unwrap();
        assert!(killed.success());
        traced(path, "bridge.ready", 2).await;
        assert_ne!(std::fs::read_to_string(fixture.home.join("peer-pid")).unwrap(), child);
        observer.wake("after respawn", vec![]);
        answered(fixture).await;
    })
    .await;
    outcome.stayed_up(1);
    assert_eq!(outcome.count("run.started"), 1);
    assert_eq!(outcome.sent(&Command::FailRun(RunFailure::Unavailable)), 0);
}
