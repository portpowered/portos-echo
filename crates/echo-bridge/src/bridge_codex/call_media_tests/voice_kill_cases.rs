//! T1: durable kill control over memory-only signaling/media/device ports.
use super::*;
use echo_link::RunFailure;
use std::cell::RefCell;

fn label(notice: &ServeNotice) -> Option<&'static str> {
    match notice {
        ServeNotice::Kill(true) => Some("killed"),
        ServeNotice::Kill(false) => Some("cleared"),
        _ => serving_label(notice),
    }
}

#[tokio::test(start_paused = true)]
#[allow(clippy::disallowed_methods, reason = "Host time for test")]
async fn kill_refuses_wake_then_stops_active_call_and_allows_fresh_call_on_clear() {
    let began = std::time::SystemTime::now();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("kill");
    std::fs::write(&path, []).unwrap();
    let (mut io, mut server) = fixture();
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let (first, first_state, _audio1) = peer(true, None);
    let (second, second_state, _audio2) = peer(true, None);
    let mut peers = std::collections::VecDeque::from([first, second]);
    let notices = RefCell::new(Vec::new());
    let (stopper, stopping) = tokio::sync::oneshot::channel::<()>();
    let mut options = serving_options();
    options.kill_path = Some(&path);
    let work = serve(
        &mut link,
        &mut io,
        options,
        async || Ok(peers.pop_front().unwrap()),
        async {
            let _ = stopping.await;
        },
        |notice| {
            if let Some(label) = label(&notice) {
                notices.borrow_mut().push(label);
            }
            Ok(())
        },
    );
    let remote = interrupted_then_reply(&mut server, began);
    let mut work = Box::pin(async {
        tokio::select! { result = work => result, () = remote => unreachable!() }
    });
    wake(&observer);
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), ["killed", "ready"]);
    assert_eq!(first_state.offers.load(Ordering::SeqCst), 0);
    assert_eq!(
        observer.commands(),
        [Command::Connect, Command::FailRun(RunFailure::KillSwitch)]
    );
    exercise_clear_and_active_kill(work.as_mut(), &path, &observer, &notices, (&first_state, &second_state)).await;
    stopper.send(()).unwrap();
    assert_eq!(work.await, Err(Error::Control));
    assert_eq!(
        observer.commands().iter().filter(|c| **c == Command::Connect).count(),
        1
    );
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| **c == Command::FailRun(RunFailure::KillSwitch))
            .count(),
        2
    );
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

async fn interrupted_then_reply(server: &mut Server, began: std::time::SystemTime) {
    admitted_start(server, began).await;
    stop(server).await;
    admitted_start(server, began).await;
    reply(server).await;
    stop(server).await;
    std::future::pending::<()>().await;
}

async fn exercise_clear_and_active_kill<F: Future>(
    mut work: Pin<&mut F>,
    path: &std::path::Path,
    observer: &Observer,
    notices: &RefCell<Vec<&str>>,
    states: (&State, &State),
) {
    let (first_state, second_state) = states;
    std::fs::remove_file(path).unwrap();
    wake(observer);
    pump(work.as_mut()).await;
    assert_eq!(
        *notices.borrow(),
        ["killed", "ready", "cleared", "listening", "connected"]
    );
    std::fs::write(path, []).unwrap();
    tokio::time::advance(Duration::from_millis(20)).await;
    pump(work.as_mut()).await;
    observer.finish(Ok(())); // Device acknowledges the cut before the next run.
    pump(work.as_mut()).await;
    assert_eq!(first_state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(notices.borrow().iter().filter(|&&s| s == "completed").count(), 0);
    assert_eq!(&notices.borrow()[5..], ["killed", "ready"]);
    std::fs::remove_file(path).unwrap();
    wake(observer);
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    assert_eq!(notices.borrow().iter().filter(|&&s| s == "completed").count(), 1);
    tokio::time::advance(Duration::from_secs(5)).await;
    pump(work.as_mut()).await;
    assert_eq!(second_state.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
#[allow(clippy::disallowed_methods, reason = "Host time for test")]
async fn kill_stop_without_acknowledgement_is_terminal_not_ready_or_retried() {
    let began = std::time::SystemTime::now();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("kill");
    let (mut io, mut server) = fixture();
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let (media, state, _audio) = peer(true, None);
    let mut media = Some(media);
    let notices = RefCell::new(Vec::new());
    let mut options = serving_options();
    options.kill_path = Some(&path);
    let work = serve(
        &mut link,
        &mut io,
        options,
        async || Ok(media.take().unwrap()),
        std::future::pending(),
        |notice| {
            if let Some(label) = label(&notice) {
                notices.borrow_mut().push(label);
            }
            Ok(())
        },
    );
    let remote = async {
        admitted_start(&mut server, began).await;
        let request = read(&mut server).await;
        assert_eq!(request["method"], "thread/realtime/stop");
        std::future::pending::<()>().await;
    };
    let mut work = Box::pin(async {
        tokio::select! { result = work => result, () = remote => unreachable!() }
    });
    wake(&observer);
    pump(work.as_mut()).await;
    std::fs::write(&path, []).unwrap();
    tokio::time::advance(Duration::from_millis(20)).await;
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(work.await, Err(Error::Deadline));
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(*notices.borrow(), ["ready", "listening", "connected"]);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn malformed_kill_marker_fails_closed_before_peer_and_clear_is_observed_without_wake() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("kill");
    std::fs::create_dir(&path).unwrap();
    let (mut io, _server) = fixture();
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let notices = RefCell::new(Vec::new());
    let (stopper, stopping) = tokio::sync::oneshot::channel::<()>();
    let mut options = serving_options();
    options.kill_path = Some(&path);
    let mut work = Box::pin(serve(
        &mut link,
        &mut io,
        options,
        async || -> Result<Controlled, Error> { unreachable!("killed wake must not create a peer") },
        async {
            let _ = stopping.await;
        },
        |notice| {
            if let Some(label) = label(&notice) {
                notices.borrow_mut().push(label);
            }
            Ok(())
        },
    ));
    wake(&observer);
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), ["killed", "ready"]);
    std::fs::remove_dir(&path).unwrap();
    tokio::time::advance(Duration::from_millis(20)).await;
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), ["killed", "ready", "cleared"]);
    stopper.send(()).unwrap();
    assert_eq!(work.await, Err(Error::Control));
    assert_eq!(
        observer.commands(),
        [Command::Connect, Command::FailRun(RunFailure::KillSwitch)]
    );
}

#[tokio::test(start_paused = true)]
#[allow(clippy::disallowed_methods, reason = "Host time for test")]
async fn kill_during_ice_closes_generation_without_connected_or_readmission() {
    let began = std::time::SystemTime::now();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("kill");
    let (mut io, mut server) = fixture();
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let (media, state, _audio) = peer(false, None);
    let mut media = Some(media);
    let notices = RefCell::new(Vec::new());
    let mut options = serving_options();
    options.kill_path = Some(&path);
    let work = serve(
        &mut link,
        &mut io,
        options,
        async || Ok(media.take().unwrap()),
        std::future::pending(),
        |notice| {
            if let Some(label) = label(&notice) {
                notices.borrow_mut().push(label);
            }
            Ok(())
        },
    );
    let remote = async {
        admitted_start(&mut server, began).await;
        std::future::pending::<()>().await;
    };
    let mut work = Box::pin(async {
        tokio::select! { result = work => result, () = remote => unreachable!() }
    });
    wake(&observer);
    pump(work.as_mut()).await;
    assert_eq!(*state.answers.lock().unwrap(), ["fixture-answer"]);
    std::fs::write(&path, []).unwrap();
    tokio::time::advance(Duration::from_millis(20)).await;
    assert_eq!(work.await, Err(Error::Unavailable));
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(state.offers.load(Ordering::SeqCst), 1);
    assert_eq!(*notices.borrow(), ["ready", "listening"]);
    assert_eq!(observer.commands(), [Command::Connect]);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
}
