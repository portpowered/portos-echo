use super::*;
use std::{
    future::Future,
    task::{Context, Waker},
};

fn ready<F: Future>(future: F) -> F::Output {
    match std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result,
        Poll::Pending => unreachable!("scripted immediate result"),
    }
}
#[test]
fn bounded_reply_resumes_sender_and_requires_acknowledgment() {
    let observer = Observer::default();
    let mut link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    let mut events = link.events().unwrap();
    assert_eq!(link.events().err(), Some(Error::Busy));
    ready(link.enable_capture(30)).unwrap();
    ready(link.probe()).unwrap();
    let (tx, rx) = link.reply_channel(NonZeroUsize::new(1).unwrap());
    ready(tx.send(vec![1, 2])).unwrap();
    let mut second = Box::pin(tx.send(vec![3, 4]));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(second.as_mut().poll(&mut cx).is_pending());
    let pcm = PcmFormat {
        rate: 24000,
        width: 2,
        channels: 1,
    };
    let mut play = Box::pin(link.play_with_timeout(pcm, rx, Duration::from_secs(2)));
    assert!(play.as_mut().poll(&mut cx).is_pending());
    assert_eq!(second.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    drop(second);
    drop(tx);
    assert!(play.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        observer.commands(),
        vec![
            Command::Connect,
            Command::Capture,
            Command::Probe,
            Command::Start(pcm, Duration::from_secs(2)),
            Command::Frame(vec![1, 2]),
            Command::Frame(vec![3, 4]),
            Command::Stop
        ]
    );
    observer.finish(Ok(()));
    assert_eq!(play.as_mut().poll(&mut cx), Poll::Ready(Ok(PlayOutcome::Played)));
    assert!(matches!(ready(events.recv()), Ok(Some(LinkEvent::Played))));
    ready(link.pause()).unwrap();
    ready(link.close()).unwrap();
    assert!(matches!(ready(events.recv()), Ok(None)));
}
#[test]
fn abandoned_source_wakes_closed_sender_and_errors_have_fixed_messages() {
    let link = ready(FakeConnector::new(Observer::default()).connect()).unwrap();
    let (tx, source) = link.reply_channel(NonZeroUsize::new(1).unwrap());
    ready(tx.send(vec![0])).unwrap();
    let mut send = Box::pin(tx.send(vec![1]));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(send.as_mut().poll(&mut cx).is_pending());
    drop(source);
    assert_eq!(send.as_mut().poll(&mut cx), Poll::Ready(Err(Error::Closed)));
    for (error, message) in [
        (Error::InvalidOptions, "invalid link options"),
        (Error::Transport, "connection lost"),
        (Error::Protocol, "invalid device frame"),
        (Error::Capacity, "bounded capacity exhausted"),
        (Error::Timeout, "operation deadline elapsed"),
        (Error::Busy, "operation already active"),
        (Error::Closed, "link closed"),
    ] {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn mute_snapshot_and_scripted_boundary_preserve_exact_frames_and_counts() {
    let observer = Observer::default();
    observer.muted_at_connect(true);
    let mut link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    let mut events = link.events().unwrap();
    assert!(matches!(ready(events.recv()).unwrap(), Some(LinkEvent::Mute(true))));
    let pcm = PcmFormat {
        rate: 16000,
        width: 2,
        channels: 1,
    };
    observer.wake_with_mute(
        "Computer",
        (0..5)
            .map(|n| MicFrame {
                format: pcm,
                payload: vec![n].into(),
            })
            .collect(),
        2,
    );
    let Some(LinkEvent::Wake(mut run)) = ready(events.recv()).unwrap() else {
        unreachable!()
    };
    let mut mute = Box::pin(events.recv());
    assert!(mute.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    for n in 0..2 {
        assert_eq!(ready(run.recv()).unwrap().unwrap().payload.as_ref(), [n]);
    }
    assert!(mute.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    assert_eq!(ready(run.recv()), Ok(None));
    assert_eq!(
        run.stats(),
        RunStats {
            received: 5,
            dropped: 3
        }
    );
    assert!(matches!(ready(mute).unwrap(), Some(LinkEvent::Mute(true))));
    assert!(matches!(
        ready(events.recv()).unwrap(),
        Some(LinkEvent::MicrophoneDropped(3))
    ));
    ready(link.close()).unwrap();
    observer.muted_at_connect(false);
    let mut fresh = ready(FakeConnector::new(observer).connect()).unwrap();
    let mut fresh_events = fresh.events().unwrap();
    assert!(
        std::pin::pin!(fresh_events.recv())
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}

#[test]
fn suppressed_idle_counts_are_payload_free_and_ordered() {
    let observer = Observer::default();
    let mut link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    let mut events = link.events().unwrap();
    observer.mute(true);
    observer.microphone_dropped(1);
    observer.microphone_dropped(3);
    observer.mute(false);
    assert!(matches!(ready(events.recv()).unwrap(), Some(LinkEvent::Mute(true))));
    assert!(matches!(
        ready(events.recv()).unwrap(),
        Some(LinkEvent::MicrophoneDropped(1))
    ));
    assert!(matches!(
        ready(events.recv()).unwrap(),
        Some(LinkEvent::MicrophoneDropped(3))
    ));
    assert!(matches!(ready(events.recv()).unwrap(), Some(LinkEvent::Mute(false))));
}

#[test]
fn run_failure_records_cause_ends_capture_and_keeps_connection() {
    let observer = Observer::default();
    let mut link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    let mut events = link.events().unwrap();
    for cause in [
        RunFailure::RelinkRequired,
        RunFailure::KillSwitch,
        RunFailure::Unavailable,
    ] {
        observer.wake("Computer", vec![]);
        let Some(LinkEvent::Wake(mut run)) = ready(events.recv()).unwrap() else {
            unreachable!()
        };
        ready(link.fail_run(cause)).unwrap();
        assert_eq!(ready(run.recv()), Ok(None));
        ready(link.probe()).unwrap();
    }
    assert_eq!(
        observer.commands(),
        vec![
            Command::Connect,
            Command::FailRun(RunFailure::RelinkRequired),
            Command::Probe,
            Command::FailRun(RunFailure::KillSwitch),
            Command::Probe,
            Command::FailRun(RunFailure::Unavailable),
            Command::Probe
        ]
    );
    ready(link.close()).unwrap();
    assert_eq!(ready(link.fail_run(RunFailure::Unavailable)), Err(Error::Closed));
}

#[test]
fn end_run_is_idempotent_drains_capture_and_keeps_future_wakes() {
    let observer = Observer::default();
    let mut link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    let mut events = link.events().unwrap();
    ready(link.end_run()).unwrap();
    for value in [1, 2] {
        observer.wake(
            "Computer",
            vec![MicFrame {
                format: PcmFormat {
                    rate: 16000,
                    width: 2,
                    channels: 1,
                },
                payload: vec![value].into(),
            }],
        );
        let Some(LinkEvent::Wake(mut run)) = ready(events.recv()).unwrap() else {
            unreachable!()
        };
        ready(link.end_run()).unwrap();
        ready(link.end_run()).unwrap();
        assert_eq!(ready(run.recv()).unwrap().unwrap().payload.as_ref(), vec![value]);
        assert_eq!(ready(run.recv()), Ok(None));
        ready(link.probe()).unwrap();
    }
    assert_eq!(
        observer.commands(),
        vec![
            Command::Connect,
            Command::EndRun,
            Command::Probe,
            Command::EndRun,
            Command::Probe
        ]
    );
    ready(link.close()).unwrap();
    assert_eq!(ready(link.end_run()), Err(Error::Closed));
}

#[test]
fn end_run_refuses_unsettled_playback_without_a_second_effect() {
    let observer = Observer::default();
    let mut link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    let mut events = link.events().unwrap();
    observer.wake("Computer", vec![]);
    assert!(matches!(ready(events.recv()), Ok(Some(LinkEvent::Wake(_)))));
    let (tx, rx) = link.reply_channel(NonZeroUsize::MIN);
    drop(tx);
    let format = PcmFormat {
        rate: 24000,
        width: 2,
        channels: 1,
    };
    let mut play = Box::pin(link.play_with_timeout(format, rx, Duration::from_secs(1)));
    assert!(play.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    assert_eq!(ready(link.end_run()), Err(Error::Busy));
    assert_eq!(
        observer.commands(),
        vec![
            Command::Connect,
            Command::Start(format, Duration::from_secs(1)),
            Command::Stop
        ]
    );
    observer.finish(Ok(()));
    assert_eq!(ready(play), Ok(PlayOutcome::Played));
    ready(link.end_run()).unwrap();
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
}

#[test]
fn invalid_capture_watchdog_has_no_device_effect() {
    let observer = Observer::default();
    let link = ready(FakeConnector::new(observer.clone()).connect()).unwrap();
    for seconds in [0, 2_147_483_648, u32::MAX] {
        assert_eq!(ready(link.enable_capture(seconds)), Err(Error::InvalidOptions));
    }
    assert_eq!(observer.commands(), vec![Command::Connect]);
    assert_eq!(ready(link.enable_capture(2_147_483_647)), Ok(()));
    assert_eq!(observer.commands(), vec![Command::Connect, Command::Capture]);
}
