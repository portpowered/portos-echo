//! In-memory link with bounded reply flow and explicit acknowledgment/failure injection.
use crate::{
    CutStats, EchoLink, Error, LinkConnector, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PcmFormat, PlayOutcome,
    ReplySink, RunFailure, RunStats,
};
use std::{
    collections::VecDeque,
    future::poll_fn,
    num::NonZeroUsize,
    sync::{Arc, Mutex, MutexGuard},
    task::{Poll, Waker},
    time::Duration,
};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Observable calls and transmitted frames in invocation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Connect,
    Capture,
    Probe,
    Start(PcmFormat, Duration),
    Frame(Vec<u8>),
    Stop,
    Cut(u64),
    EndRun,
    FailRun(RunFailure),
    Pause,
    Close,
}
#[derive(Default)]
struct State {
    commands: Vec<Command>,
    initial_mute: Option<()>,
    mute_drain: Option<Arc<Mutex<bool>>>,
    events: VecDeque<LinkEvent<FakeRun>>,
    event_waker: Option<Waker>,
    play_waker: Option<Waker>,
    outcome: Option<Result<(), Error>>,
    cut: bool,
    stopped: bool,
    run_open: Option<()>,
    source: Option<Arc<Mutex<Queue>>>,
    closed: bool,
}
/// An explicit script/observer; contains no executor, socket or clock.
#[derive(Clone, Default)]
pub struct Observer(Arc<Mutex<State>>);
impl Observer {
    #[must_use]
    pub fn commands(&self) -> Vec<Command> {
        lock(&self.0).commands.clone()
    }
    pub fn wake(&self, phrase: &str, frames: Vec<MicFrame>) {
        let mut state = lock(&self.0);
        state.events.push_back(LinkEvent::Wake(FakeRun {
            phrase: phrase.into(),
            stats: RunStats {
                received: frames.len() as u64,
                dropped: 0,
            },
            frames: frames.into(),
            observer: self.clone(),
            mute_end: None,
        }));
        if let Some(waker) = state.event_waker.take() {
            waker.wake();
        }
    }
    /// Set the device state sampled by the next connection.
    pub fn muted_at_connect(&self, muted: bool) {
        lock(&self.0).initial_mute = muted.then_some(());
    }
    /// Script an idle mute transition in event order.
    pub fn mute(&self, muted: bool) {
        let mut state = lock(&self.0);
        state.initial_mute = muted.then_some(());
        state.events.push_back(LinkEvent::Mute(muted));
        if let Some(waker) = state.event_waker.take() {
            waker.wake();
        }
    }
    /// Script suppressed idle chunks without retaining their audio.
    pub fn microphone_dropped(&self, count: u64) {
        let mut state = lock(&self.0);
        state.events.push_back(LinkEvent::MicrophoneDropped(count));
        if let Some(waker) = state.event_waker.take() {
            waker.wake();
        }
    }
    /// Script a mute between frame positions; later frames are counted and dropped.
    pub fn wake_with_mute(&self, phrase: &str, mut frames: Vec<MicFrame>, after: usize) {
        let received = frames.len() as u64;
        frames.truncate(after);
        let dropped = received - frames.len() as u64;
        let ended = Arc::new(Mutex::new(false));
        let mut state = lock(&self.0);
        state.mute_drain = Some(ended.clone());
        state.initial_mute = Some(());
        state.events.push_back(LinkEvent::Wake(FakeRun {
            phrase: phrase.into(),
            frames: frames.into(),
            stats: RunStats { received, dropped },
            observer: self.clone(),
            mute_end: Some(ended),
        }));
        state.events.push_back(LinkEvent::Mute(true));
        state.events.push_back(LinkEvent::MicrophoneDropped(dropped));
        if let Some(waker) = state.event_waker.take() {
            waker.wake();
        }
    }
    /// Deliver the actual acknowledgment, or an operation's scripted terminal failure.
    pub fn finish(&self, result: Result<(), Error>) {
        let mut state = lock(&self.0);
        state.outcome = Some(result);
        if result.is_ok() {
            state.events.push_back(LinkEvent::Played);
        }
        if let Some(waker) = state.event_waker.take() {
            waker.wake();
        }
        if let Some(waker) = state.play_waker.take() {
            waker.wake();
        }
    }
}
/// One link owner; taking events twice is rejected.
pub struct FakeLink {
    observer: Observer,
    taken: bool,
}
/// In-memory connector; reconnect is another explicit connect invocation.
pub struct FakeConnector(Observer);
impl FakeConnector {
    #[must_use]
    pub fn new(observer: Observer) -> Self {
        Self(observer)
    }
}
impl LinkConnector for FakeConnector {
    type Link = FakeLink;
    fn connect(&self) -> impl std::future::Future<Output = Result<FakeLink, Error>> + Send {
        std::future::ready({
            let mut state = lock(&self.0.0);
            state.commands.push(Command::Connect);
            state.closed = false;
            if state.initial_mute.is_some() {
                state.events.push_front(LinkEvent::Mute(true));
            }
            drop(state);
            Ok(FakeLink {
                observer: self.0.clone(),
                taken: false,
            })
        })
    }
}
/// Scripted run frames drain before EOF; statistics belong to this run.
pub struct FakeRun {
    phrase: String,
    frames: VecDeque<MicFrame>,
    stats: RunStats,
    observer: Observer,
    mute_end: Option<Arc<Mutex<bool>>>,
}
impl Drop for FakeRun {
    fn drop(&mut self) {
        if let Some(ended) = &self.mute_end {
            *lock(ended) = true;
            if let Some(waker) = lock(&self.observer.0).event_waker.take() {
                waker.wake();
            }
        }
    }
}
impl MicrophoneRun for FakeRun {
    fn wake_phrase(&self) -> &str {
        &self.phrase
    }
    async fn recv(&mut self) -> Result<Option<MicFrame>, Error> {
        if let Some(frame) = self.frames.pop_front() {
            return Ok(Some(frame));
        }
        if let Some(ended) = &self.mute_end {
            *lock(ended) = true;
            if let Some(waker) = lock(&self.observer.0).event_waker.take() {
                waker.wake();
            }
            return Ok(None);
        }
        poll_fn(|cx| {
            let mut state = lock(&self.observer.0);
            if state.stopped {
                return Poll::Ready(Ok(None));
            }
            state.play_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
    fn stats(&self) -> RunStats {
        self.stats
    }
}
/// Sole event receiver for the script.
pub struct Events(Observer);
impl LinkEvents for Events {
    type Run = FakeRun;
    async fn recv(&mut self) -> Result<Option<LinkEvent<FakeRun>>, Error> {
        poll_fn(|cx| {
            let mut state = lock(&self.0.0);
            if matches!(state.events.front(), Some(LinkEvent::Mute(true)))
                && state.mute_drain.as_ref().is_some_and(|ended| !*lock(ended))
            {
                state.event_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            if let Some(event) = state.events.pop_front() {
                if matches!(event, LinkEvent::Wake(_)) {
                    state.run_open = Some(());
                    state.stopped = false;
                }
                return Poll::Ready(Ok(Some(event)));
            }
            if state.closed {
                return Poll::Ready(Ok(None));
            }
            if let Some(Err(error)) = state.outcome {
                return Poll::Ready(Err(error));
            }
            state.event_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}
struct Queue {
    frames: VecDeque<Vec<u8>>,
    capacity: usize,
    senders: usize,
    receiver: bool,
    send_waker: Option<Waker>,
    recv_waker: Option<Waker>,
}
/// Bounded producer; blocked sends resume when playback consumes a frame.
pub struct Sink(Arc<Mutex<Queue>>);
impl Drop for Sink {
    fn drop(&mut self) {
        let mut queue = lock(&self.0);
        queue.senders -= 1;
        if let Some(waker) = queue.recv_waker.take() {
            waker.wake();
        }
    }
}
/// The adapter-owned receiver; never adds a second pump or executor.
pub struct Source(Arc<Mutex<Queue>>);
impl Drop for Source {
    fn drop(&mut self) {
        let mut queue = lock(&self.0);
        queue.receiver = false;
        if let Some(waker) = queue.send_waker.take() {
            waker.wake();
        }
    }
}
impl ReplySink for Sink {
    async fn send(&self, frame: Vec<u8>) -> Result<(), Error> {
        let mut frame = Some(frame);
        poll_fn(|cx| {
            let mut queue = lock(&self.0);
            if !queue.receiver {
                return Poll::Ready(Err(Error::Closed));
            }
            if queue.frames.len() == queue.capacity {
                queue.send_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            if let Some(frame) = frame.take() {
                queue.frames.push_back(frame);
            }
            if let Some(waker) = queue.recv_waker.take() {
                waker.wake();
            }
            Poll::Ready(Ok(()))
        })
        .await
    }
}
impl EchoLink for FakeLink {
    type Events = Events;
    type ReplySink = Sink;
    type ReplySource = Source;
    fn events(&mut self) -> Result<Events, Error> {
        if self.taken {
            return Err(Error::Busy);
        }
        self.taken = true;
        Ok(Events(self.observer.clone()))
    }
    fn reply_channel(&self, capacity: NonZeroUsize) -> (Sink, Source) {
        let queue = Arc::new(Mutex::new(Queue {
            frames: VecDeque::new(),
            capacity: capacity.get(),
            senders: 1,
            receiver: true,
            send_waker: None,
            recv_waker: None,
        }));
        (Sink(queue.clone()), Source(queue))
    }
    fn enable_capture(&self, failsafe_seconds: u32) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        std::future::ready(if failsafe_seconds == 0 || failsafe_seconds > i32::MAX as u32 {
            Err(Error::InvalidOptions)
        } else {
            lock(&self.observer.0).commands.push(Command::Capture);
            Ok(())
        })
    }
    fn probe(&self) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        std::future::ready({
            lock(&self.observer.0).commands.push(Command::Probe);
            Ok(())
        })
    }
    async fn play_with_timeout(
        &self,
        format: PcmFormat,
        source: Source,
        deadline: Duration,
    ) -> Result<PlayOutcome, Error> {
        {
            let mut state = lock(&self.observer.0);
            if deadline.is_zero() {
                return Err(Error::InvalidOptions);
            }
            if state.closed {
                return Err(Error::Closed);
            }
            if state.source.is_some() {
                return Err(Error::Busy);
            }
            state.commands.push(Command::Start(format, deadline));
            state.source = Some(source.0.clone());
            state.cut = false;
            state.stopped = false;
            state.outcome = None;
        }
        poll_fn(|cx| {
            let mut state = lock(&self.observer.0);
            let mut queue = lock(&source.0);
            while let Some(frame) = queue.frames.pop_front() {
                if !state.cut {
                    state.commands.push(Command::Frame(frame));
                }
                if let Some(waker) = queue.send_waker.take() {
                    waker.wake();
                }
            }
            if (queue.senders == 0 || state.cut) && !state.stopped {
                state.commands.push(Command::Stop);
                state.stopped = true;
            }
            if let Some(result) = state.outcome {
                state.source = None;
                return Poll::Ready(result.map(|()| {
                    if state.cut {
                        PlayOutcome::Cut
                    } else {
                        PlayOutcome::Played
                    }
                }));
            }
            state.play_waker = Some(cx.waker().clone());
            queue.recv_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
    fn cut_with_stats(&self) -> impl std::future::Future<Output = Result<CutStats, Error>> + Send {
        std::future::ready({
            let mut state = lock(&self.observer.0);
            let mut dropped = 0;
            if !state.cut {
                state.cut = true;
                if let Some(source) = &state.source {
                    let mut queue = lock(source);
                    dropped = queue.frames.len() as u64;
                    queue.frames.clear();
                    if let Some(waker) = queue.send_waker.take() {
                        waker.wake();
                    }
                }
                state.commands.push(Command::Cut(dropped));
            }
            if let Some(waker) = state.play_waker.take() {
                waker.wake();
            }
            Ok(CutStats {
                dropped_frames: dropped,
            })
        })
    }
    fn end_run(&self) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        let mut state = lock(&self.observer.0);
        if state.closed {
            return std::future::ready(Err(Error::Closed));
        }
        if state.source.is_some() {
            return std::future::ready(Err(Error::Busy));
        }
        if state.run_open.take().is_some() {
            state.commands.push(Command::EndRun);
            state.stopped = true;
            if let Some(waker) = state.play_waker.take() {
                waker.wake();
            }
        }
        std::future::ready(Ok(()))
    }
    fn pause(&self) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        std::future::ready({
            lock(&self.observer.0).commands.push(Command::Pause);
            Ok(())
        })
    }
    fn fail_run(&self, code: RunFailure) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        let mut state = lock(&self.observer.0);
        if state.closed {
            return std::future::ready(Err(Error::Closed));
        }
        state.commands.push(Command::FailRun(code));
        state.run_open = None;
        state.stopped = true;
        if let Some(waker) = state.play_waker.take() {
            waker.wake();
        }
        std::future::ready(Ok(()))
    }
    fn close(&self) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        std::future::ready({
            let mut state = lock(&self.observer.0);
            state.commands.push(Command::Close);
            state.closed = true;
            if let Some(waker) = state.event_waker.take() {
                waker.wake();
            }
            if let Some(waker) = state.play_waker.take() {
                waker.wake();
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "fake/link_tests.rs"]
mod tests;
