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
    ready(link.enable_capture()).unwrap();
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
