//! One device connection outlives the authenticated generations that use it.
use echo_link::{CutStats, EchoLink, Error, LinkConnector, LinkEvent, LinkEvents, PcmFormat, PlayOutcome, RunFailure};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, sleep_until, timeout};

type Run<L> = <<L as EchoLink>::Events as LinkEvents>::Run;

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Kept<L: EchoLink> {
    link: L,
    /// The single event stream, lent to one generation or one rest at a time.
    events: Mutex<Option<L::Events>>,
    /// A wake that arrived between generations; the next one admits it first.
    pending: Mutex<Option<Run<L>>>,
    captured: AtomicBool,
    muted: AtomicBool,
    /// The device has a run that was neither ended nor failed.
    open: AtomicBool,
}

/// Connects on the first admission, after authentication, and then lends the
/// same connection to every later generation. A generation's close is a no-op:
/// the device keeps its registration, so a wake between generations is held
/// and answered instead of finding no link.
pub(in crate::bridge_voice) struct Retained<C: LinkConnector> {
    connector: C,
    kept: Mutex<Option<Arc<Kept<C::Link>>>>,
}

impl<C: LinkConnector> Retained<C> {
    pub fn new(connector: C) -> Self {
        Self {
            connector,
            kept: Mutex::new(None),
        }
    }

    fn current(&self) -> Option<Arc<Kept<C::Link>>> {
        lock(&self.kept).clone()
    }

    /// Device mute known to the kept connection; None before any connection.
    pub fn muted(&self) -> Option<bool> {
        self.current().map(|kept| kept.muted.load(Ordering::SeqCst))
    }

    /// After a failed generation: end the device's open or held run with the
    /// existing error, then prove the connection. A link that cannot do both,
    /// or that the caller no longer trusts, is closed and the next generation
    /// reconnects. Returns whether it is kept.
    pub async fn settle(&self, code: RunFailure, trusted: bool) -> bool {
        let Some(kept) = self.current() else { return false };
        drop(lock(&kept.pending).take());
        let ended = async {
            if kept.open.swap(false, Ordering::SeqCst) {
                kept.link.fail_run(code).await?;
            }
            kept.link.probe().await
        };
        if matches!(timeout(Duration::from_secs(5), ended).await, Ok(Ok(()))) && trusted {
            return true;
        }
        self.close().await;
        false
    }

    /// Wait out a retry delay while still listening to the device. A wake ends
    /// the wait at once and is held for the generation it starts.
    pub async fn rest(&self, delay: Duration, mut observe: impl FnMut(LinkEvent<()>)) {
        let until = Instant::now() + delay;
        let events = self.current().and_then(|kept| Shared::lend(&kept));
        if let Some(mut events) = events {
            loop {
                let event = tokio::select! {
                    biased;
                    () = sleep_until(until) => return,
                    event = events.recv() => event,
                };
                match event {
                    Ok(Some(LinkEvent::Wake(run))) => {
                        *lock(&events.kept.pending) = Some(run);
                        return;
                    }
                    Ok(Some(LinkEvent::Mute(value))) => observe(LinkEvent::Mute(value)),
                    Ok(Some(LinkEvent::MicrophoneDropped(count))) => observe(LinkEvent::MicrophoneDropped(count)),
                    Ok(Some(LinkEvent::Played | LinkEvent::Other)) => (),
                    Ok(None) | Err(_) => break,
                }
            }
            drop(events);
            self.close().await;
        }
        sleep_until(until).await;
    }

    /// Close the real connection. Best effort and bounded, like the host's own.
    pub async fn close(&self) {
        let kept = lock(&self.kept).take();
        if let Some(kept) = kept {
            let _ = timeout(Duration::from_secs(2), kept.link.close()).await;
        }
    }
}

impl<C: LinkConnector> LinkConnector for Retained<C> {
    type Link = Handle<C::Link>;

    async fn connect(&self) -> Result<Self::Link, Error> {
        if let Some(kept) = self.current() {
            return Ok(Handle(kept));
        }
        let mut link = self.connector.connect().await?;
        let events = link.events()?;
        let kept = Arc::new(Kept {
            link,
            events: Mutex::new(Some(events)),
            pending: Mutex::new(None),
            captured: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            open: AtomicBool::new(false),
        });
        *lock(&self.kept) = Some(kept.clone());
        Ok(Handle(kept))
    }
}

/// One generation's borrow of the kept connection.
pub(in crate::bridge_voice) struct Handle<L: EchoLink>(Arc<Kept<L>>);

impl<L: EchoLink> EchoLink for Handle<L> {
    type Events = Shared<L>;
    type ReplySink = L::ReplySink;
    type ReplySource = L::ReplySource;

    fn events(&mut self) -> Result<Self::Events, Error> {
        Shared::lend(&self.0).ok_or(Error::Busy)
    }
    fn reply_channel(&self, capacity: NonZeroUsize) -> (Self::ReplySink, Self::ReplySource) {
        self.0.link.reply_channel(capacity)
    }
    async fn enable_capture(&self, failsafe_seconds: u32) -> Result<(), Error> {
        if self.0.captured.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.0.link.enable_capture(failsafe_seconds).await?;
        self.0.captured.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn probe(&self) -> Result<(), Error> {
        self.0.link.probe().await
    }
    async fn play_with_timeout(
        &self,
        format: PcmFormat,
        source: Self::ReplySource,
        deadline: Duration,
    ) -> Result<PlayOutcome, Error> {
        self.0.link.play_with_timeout(format, source, deadline).await
    }
    async fn cut_with_stats(&self) -> Result<CutStats, Error> {
        self.0.link.cut_with_stats().await
    }
    async fn end_run(&self) -> Result<(), Error> {
        self.0.link.end_run().await?;
        self.0.open.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn fail_run(&self, code: RunFailure) -> Result<(), Error> {
        self.0.link.fail_run(code).await?;
        self.0.open.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn pause(&self) -> Result<(), Error> {
        self.0.link.pause().await
    }
    async fn thinking(&self, on: bool) -> Result<(), Error> {
        self.0.link.thinking(on).await
    }
    /// The connection belongs to the bridge, not to this generation.
    fn close(&self) -> impl std::future::Future<Output = Result<(), Error>> {
        std::future::ready(Ok(()))
    }
}

/// The kept event stream; dropping it returns the stream for the next borrower.
pub(in crate::bridge_voice) struct Shared<L: EchoLink> {
    inner: Option<L::Events>,
    kept: Arc<Kept<L>>,
}

impl<L: EchoLink> Shared<L> {
    fn lend(kept: &Arc<Kept<L>>) -> Option<Self> {
        let inner = lock(&kept.events).take()?;
        Some(Self {
            inner: Some(inner),
            kept: kept.clone(),
        })
    }
}

impl<L: EchoLink> Drop for Shared<L> {
    fn drop(&mut self) {
        *lock(&self.kept.events) = self.inner.take();
    }
}

impl<L: EchoLink> LinkEvents for Shared<L> {
    type Run = Run<L>;

    async fn recv(&mut self) -> Result<Option<LinkEvent<Self::Run>>, Error> {
        let held = lock(&self.kept.pending).take();
        if let Some(run) = held {
            return Ok(Some(LinkEvent::Wake(run)));
        }
        let event = self.inner.as_mut().ok_or(Error::Closed)?.recv().await?;
        match &event {
            Some(LinkEvent::Mute(value)) => self.kept.muted.store(*value, Ordering::SeqCst),
            // A muted wake is dropped by every owner without a device run.
            Some(LinkEvent::Wake(_)) if !self.kept.muted.load(Ordering::SeqCst) => {
                self.kept.open.store(true, Ordering::SeqCst);
            }
            _ => (),
        }
        Ok(event)
    }
}
