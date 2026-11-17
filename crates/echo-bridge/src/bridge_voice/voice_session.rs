//! One connected call spans wakes; every return awaits signaling/media cleanup.
use super::{Notice, Options, Stats, run};
use crate::{
    Error,
    bridge_codex::{Event, MediaCall, MediaEvent},
};
use echo_codex::{DynamicTool, codex_io::RpcIo};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use echo_media::MediaPeer;
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout_at},
};

#[derive(Clone, Copy)]
pub struct SessionOptions {
    pub idle: Duration,
    pub response_timeout: Duration,
    pub played_timeout: Duration,
    pub reply_lead_ms: u16,
    pub muted: bool,
}

/// Observations contain no provider bodies, SDP or credentials.
pub enum SessionNotice {
    Connected,
    Started,
    Run(Notice),
    Completed(Stats),
}

pub struct ConversationOptions<'a> {
    pub call: crate::bridge_codex::Options<'a>,
    pub tools: Vec<DynamicTool>,
    pub deadline: Instant,
    pub session: SessionOptions,
}

/// Negotiate media on the host's initialized IO and drive its admitted wake.
/// Connected is emitted only after signaling AND media readiness. Cancellation
/// takes priority over admission; every return awaits call/media cleanup. The
/// host must still close the device and reap its supervisor after this returns.
///
/// # Errors
/// Startup, observation and conversation failures retain their original cause
/// after cleanup. No restart, legacy fallback or call replay is attempted.
pub async fn open_conversation<L, R, W, P>(
    device: (&L, &mut L::Events, <L::Events as LinkEvents>::Run),
    io: &mut RpcIo<R, W>,
    peer: P,
    options: ConversationOptions<'_>,
    cancelled: impl Future<Output = ()>,
    mut observe: impl FnMut(SessionNotice) -> Result<(), Error>,
) -> Result<u64, Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    let mut call = MediaCall::new(io, peer);
    tokio::pin!(cancelled);
    let work = async {
        // Refuse an already expired deadline before generating an offer or RPC.
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            result = async {
                if Instant::now() >= options.deadline { return Err(Error::Deadline); }
                validate(&options.session)?;
                call.start(&options.call, options.tools, options.deadline).await
            } => result?,
        }
        observe(SessionNotice::Connected)?;
        conversation(device, &mut call, options.session, cancelled.as_mut(), &mut observe).await
    };
    let result = work.await;
    // Also covers a cancelled start and a failed Connected observer. The
    // conversation's own completed cleanup is idempotent here.
    let cleanup = call.close(Instant::now() + Duration::from_secs(2)).await;
    settled(result, cleanup)
}

/// Drive an already connected call, beginning with its admitted microphone run.
/// The caller publishes Connected only after `MediaCall::start` has succeeded.
/// A healthy played reply retains the call until the next wake or idle expiry.
/// Expiry wins over a wake at equality; muted wakes cannot renew the deadline.
/// Idle media is drained and never forwarded into a later run.
///
/// All returns, including cancellation and observer errors, await call stop and
/// media close. The caller still owns device close and supervisor reaping.
/// Pass cancellation here, inside the host callback, to permit graceful stop
/// before the host drops the IO borrower. No tools are executed by this owner.
///
/// # Errors
/// Invalid durations, failed run/control/media/device, cancellation, unexpected
/// tool requests or cleanup failure. No automatic restart or replay occurs.
pub async fn conversation<L, R, W, P>(
    device: (&L, &mut L::Events, <L::Events as LinkEvents>::Run),
    call: &mut MediaCall<'_, R, W, P>,
    mut options: SessionOptions,
    cancelled: impl Future<Output = ()>,
    mut observe: impl FnMut(SessionNotice) -> Result<(), Error>,
) -> Result<u64, Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    tokio::pin!(cancelled);
    let work = async {
        validate(&options)?;
        drive(device, call, &mut options, cancelled.as_mut(), &mut observe).await
    };
    let result = work.await;
    // This cleanup is deliberately outside the cancelled operation. A ready
    // cancellation or observer error must not drop an active media owner.
    let cleanup = call.close(Instant::now() + Duration::from_secs(2)).await;
    settled(result, cleanup)
}

fn settled(result: Result<u64, Error>, cleanup: Result<(), Error>) -> Result<u64, Error> {
    // A kill may resume only after successful cleanup. Preserve other original
    // failures, but never normalize cancellation over an unsuccessful stop.
    if result == Err(Error::Control) {
        cleanup.and(result)
    } else {
        result.and_then(|completed| cleanup.map(|()| completed))
    }
}

pub(super) fn validate(options: &SessionOptions) -> Result<(), Error> {
    if options.response_timeout.is_zero() || options.played_timeout.is_zero() {
        return Err(Error::Input);
    }
    Instant::now()
        .checked_add(options.response_timeout)
        .and_then(|deadline| deadline.checked_add(options.played_timeout))
        .ok_or(Error::Input)?;
    Ok(())
}

async fn drive<L, R, W, P>(
    device: (&L, &mut L::Events, <L::Events as LinkEvents>::Run),
    call: &mut MediaCall<'_, R, W, P>,
    options: &mut SessionOptions,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(SessionNotice) -> Result<(), Error>,
) -> Result<u64, Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    let (client, events, mut microphone) = device;
    tokio::pin!(cancelled);
    let mut completed = 0_u64;
    loop {
        let deadline = Instant::now()
            .checked_add(options.response_timeout)
            .ok_or(Error::Input)?;
        deadline.checked_add(options.played_timeout).ok_or(Error::Input)?;
        observe(SessionNotice::Started)?;
        let stats = run(
            client,
            events,
            microphone,
            call,
            Options {
                response_deadline: deadline,
                played_timeout: options.played_timeout,
                reply_lead_ms: options.reply_lead_ms,
                muted: options.muted,
            },
            cancelled.as_mut(),
            |notice| {
                if let Notice::Device(LinkEvent::Mute(muted)) = &notice {
                    options.muted = *muted;
                }
                observe(SessionNotice::Run(notice))
            },
        )
        .await?;
        completed = completed.checked_add(1).ok_or(Error::Unavailable)?;
        observe(SessionNotice::Completed(stats))?;
        let expires = Instant::now().checked_add(options.idle);
        let next = wait(events, call, expires, &mut options.muted, cancelled.as_mut(), observe).await?;
        let Some(next) = next else {
            return Ok(completed);
        };
        microphone = next;
        // The queue may contain the preceding reply's late idle PCM when the
        // wake wins select. Drain accepted frames before admitting capture.
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            result = timeout_at(Instant::now().checked_add(options.response_timeout).ok_or(Error::Input)?, clear_idle(call)) => {
                result.map_err(|_| Error::Deadline)??;
            }
        }
    }
}

async fn wait<E, R, W, P>(
    events: &mut E,
    call: &mut MediaCall<'_, R, W, P>,
    expires: Option<Instant>,
    muted: &mut bool,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(SessionNotice) -> Result<(), Error>,
) -> Result<Option<E::Run>, Error>
where
    E: LinkEvents,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    tokio::pin!(cancelled);
    loop {
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            () = expiry(expires) => return Ok(None),
            result = idle_step(events, call, muted, observe) => {
                if let Some(run) = result? { return Ok(Some(run)); }
            }
        }
        // Bound ready-event bursts so the enclosing host can observe shutdown.
        tokio::task::yield_now().await;
    }
}

async fn idle_step<E, R, W, P>(
    events: &mut E,
    call: &mut MediaCall<'_, R, W, P>,
    muted: &mut bool,
    observe: &mut impl FnMut(SessionNotice) -> Result<(), Error>,
) -> Result<Option<E::Run>, Error>
where
    E: LinkEvents,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    // Device traffic must not starve the app-server pipes or media worker.
    tokio::select! {
        event = events.recv() => match event?.ok_or(Error::Wyoming)? {
            LinkEvent::Wake(run) if !*muted => return Ok(Some(run)),
            LinkEvent::Wake(_) | LinkEvent::Played | LinkEvent::Other => (),
            LinkEvent::Mute(value) => {
                *muted = value;
                observe(SessionNotice::Run(Notice::Device(LinkEvent::Mute(value))))?;
            }
            LinkEvent::MicrophoneDropped(count) => {
                observe(SessionNotice::Run(Notice::Device(LinkEvent::MicrophoneDropped(count))))?;
            }
        },
        event = call.next() => match event? {
            MediaEvent::Control(Event::Closed | Event::Tool { .. }) => return Err(Error::Unavailable),
            // Neither an idle transcript nor old PCM starts a new reply.
            MediaEvent::Control(_) | MediaEvent::Audio(_) => (),
        },
    }
    Ok(None)
}

async fn expiry(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn clear_idle<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer>(
    call: &mut MediaCall<'_, R, W, P>,
) -> Result<(), Error> {
    while call.ready_audio().await?.is_some() {
        tokio::task::yield_now().await;
    }
    Ok(())
}
