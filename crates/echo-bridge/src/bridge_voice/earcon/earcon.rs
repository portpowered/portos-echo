//! A scoped, bounded speaker owner, independent of reply audio and mouth state.
mod capture;
#[cfg(test)]
mod discovery_tests;
#[cfg(test)]
mod tests;
mod waveform;
pub(super) use capture::{during as capture_during, recv as microphone};

use crate::Error;
use echo_link::{EchoLink, LinkEvent, LinkEvents, PlayOutcome, ReplySink};
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::Notify,
    time::{Instant, timeout},
};

const FRAME: Duration = Duration::from_millis(20);
// Same device playout delay used by voice_source::Mouth. This is deliberately
// independent of that reply-only model: no earcon samples ever enter Mouth.
const OUTPUT_DELAY: Duration = Duration::from_millis(200);
const SETTLE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    phase: Phase,
    stop: bool,
    receipt: bool,
    closing: bool,
    completed: Option<Duration>,
    capture: capture::Capture,
}

#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Active,
    Failed,
}

#[derive(Clone, Default)]
struct Context {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

tokio::task_local! { static CURRENT: Context; }

impl Context {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn until(&self, condition: impl Fn(&State) -> bool) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if condition(&self.state()) {
                return;
            }
            changed.await;
        }
    }
}

fn current() -> Option<Context> {
    CURRENT.try_with(Clone::clone).ok()
}

pub(super) fn active() -> bool {
    current().is_some_and(|context| context.state().phase == Phase::Active)
}

pub(super) fn start() -> Result<(), Error> {
    if let Some(context) = current() {
        let mut state = context.state();
        if state.phase != Phase::Idle || state.completed.is_some() || state.closing {
            return Err(Error::Wyoming);
        }
        state.capture = capture::Capture::default();
        state.phase = Phase::Active;
        state.stop = false;
        state.receipt = false;
        context.changed.notify_waiters();
    }
    Ok(())
}

pub(super) fn preempt() {
    if let Some(context) = current() {
        context.state().stop = true;
        context.changed.notify_waiters();
    }
}

pub(super) async fn idle() -> Result<(), Error> {
    if let Some(context) = current() {
        context.until(|state| state.phase != Phase::Active).await;
        if context.state().phase == Phase::Failed {
            return Err(Error::Wyoming);
        }
    }
    Ok(())
}

pub(super) async fn failed() {
    if let Some(context) = current() {
        context.until(|state| state.phase == Phase::Failed).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Only the existing event consumer calls this, including during negotiation.
/// The first Played belongs to this operation until both receipts settle.
pub(super) fn receipt<R>(event: &LinkEvent<R>) -> bool {
    let Some(context) = current() else {
        return false;
    };
    if matches!(event, LinkEvent::Mute(true)) {
        preempt();
    }
    let mut state = context.state();
    if matches!(event, LinkEvent::Played) && state.phase == Phase::Active && !state.receipt {
        state.receipt = true;
        context.changed.notify_waiters();
        return true;
    }
    false
}

pub(super) async fn completed() -> Duration {
    let Some(context) = current() else {
        return std::future::pending::<Duration>().await;
    };
    loop {
        context.until(|state| state.completed.is_some()).await;
        if let Some(duration) = context.state().completed.take() {
            return duration;
        }
    }
}

/// Poll the one event reader while setup owns the media port. No microphone
/// payload is taken by this event reader; `capture_during` retains setup PCM.
pub(super) async fn negotiating<E: LinkEvents, T>(
    events: &mut E,
    work: impl Future<Output = Result<T, Error>>,
    observe: &mut impl FnMut(super::SessionNotice) -> Result<(), Error>,
) -> Result<T, Error> {
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            duration = completed() => observe(super::SessionNotice::Run(super::Notice::EarconPlayed(duration)))?,
            event = events.recv(), if current().is_some() => {
                let event = event?.ok_or(Error::Wyoming)?;
                if receipt(&event) { continue; }
                let event = match event {
                    LinkEvent::Mute(value) => LinkEvent::Mute(value),
                    LinkEvent::MicrophoneDropped(count) => LinkEvent::MicrophoneDropped(count),
                    LinkEvent::Other => LinkEvent::Other,
                    LinkEvent::StaleRemoteCapture | LinkEvent::RemoteCapture { .. } => LinkEvent::StaleRemoteCapture,
                    LinkEvent::Wake(_) | LinkEvent::Played => return Err(Error::Wyoming),
                };
                let muted = matches!(event, LinkEvent::Mute(true));
                observe(super::SessionNotice::Run(super::Notice::Device(event)))?;
                if muted { return Err(Error::Control); }
            }
            result = &mut work => return result,
        }
    }
}

pub(super) async fn scope<L: EchoLink>(
    client: &L,
    enabled: bool,
    work: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    if !enabled {
        return work.await;
    }
    let context = Context::default();
    CURRENT
        .scope(context.clone(), async {
            let borrower = async {
                let result = work.await;
                context.state().closing = true;
                context.changed.notify_waiters();
                result
            };
            let (result, cleanup) = tokio::join!(borrower, supervise(client, &context));
            result.and(cleanup)
        })
        .await
}

async fn supervise(client: &impl EchoLink, context: &Context) -> Result<(), Error> {
    loop {
        context
            .until(|state| state.phase == Phase::Active || state.closing)
            .await;
        if context.state().phase != Phase::Active {
            return Ok(());
        }
        let result = play(client, context).await;
        if result.is_err() {
            context.state().phase = Phase::Failed;
            context.changed.notify_waiters();
            context.until(|state| state.closing).await;
            return result;
        }
        context.state().phase = Phase::Idle;
        context.changed.notify_waiters();
    }
}

async fn play(client: &impl EchoLink, context: &Context) -> Result<(), Error> {
    let (sink, source) = client.reply_channel(NonZeroUsize::MIN);
    let playback = timeout(
        SETTLE,
        client.play_with_timeout(crate::bridge_audio::format(24_000), source, SETTLE),
    );
    let producer = produce(client, &sink, context);
    // Keep the sender alive through the deliberate capture-preserving cut.
    // Even on producer failure the operation is bounded and joined.
    let (outcome, duration) = tokio::join!(playback, producer);
    let duration = duration?;
    if outcome.map_err(|_| Error::Deadline)?? != PlayOutcome::Cut {
        return Err(Error::Wyoming);
    }
    timeout(SETTLE, context.until(|state| state.receipt || state.closing))
        .await
        .map_err(|_| Error::Deadline)?;
    let mut state = context.state();
    if state.receipt && !state.closing {
        state.completed = Some(duration);
    }
    Ok(())
}

async fn produce(client: &impl EchoLink, sink: &impl ReplySink, context: &Context) -> Result<Duration, Error> {
    let origin = Instant::now();
    let waveform = waveform::samples();
    let mut sent = 0_u32;
    for frame in waveform.as_chunks::<480>().0 {
        let frame = frame.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        tokio::select! {
            biased;
            () = context.until(|state| state.stop || state.closing) => break,
            result = async {
                // A wake between timer ticks must not defer its first chunk.
                if sent != 0 {
                    tokio::time::sleep_until(origin + FRAME * sent).await;
                }
                timeout(SETTLE, sink.send(frame)).await.map_err(|_| echo_link::Error::Timeout)?
            } => result?,
        }
        if let Some(clock) = crate::bridge_mcp::bridge_clock::receipt::Context::current() {
            clock.submitted("earcon")?;
        }
        sent += 1;
    }
    tokio::select! {
        biased;
        () = context.until(|state| state.stop || state.closing) => (),
        () = tokio::time::sleep_until(origin + FRAME * sent + OUTPUT_DELAY) => (),
    }
    let cut = timeout(SETTLE, client.cut_with_stats())
        .await
        .map_err(|_| Error::Deadline)??;
    if let Some(clock) = crate::bridge_mcp::bridge_clock::receipt::Context::current() {
        clock.discarded("earcon", cut.dropped_frames)?;
    }
    let dropped = u32::try_from(cut.dropped_frames).map_err(|_| Error::Wyoming)?;
    Ok(FRAME * sent.checked_sub(dropped).ok_or(Error::Wyoming)?)
}
