//! Idle supervision and first-wake admission on the authenticated IO borrower.
use super::{Notice, SessionNotice, SessionOptions, voice_kill::Gate};
use crate::{Error, bridge_codex};
use echo_codex::{DynamicTool, codex_io::RpcIo, codex_protocol::Message};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use echo_media::MediaPeer;
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout, timeout_at},
};

#[derive(Clone)]
pub struct ServeOptions<'a> {
    pub kill_path: Option<&'a std::path::Path>,
    pub call: bridge_codex::Options<'a>,
    pub tools: Vec<DynamicTool>,
    pub session: SessionOptions,
    pub connect_timeout: Duration,
}

/// Ready means authenticated device readiness, never media connectivity.
pub enum ServeNotice {
    Ready,
    Kill(bool),
    Listening,
    Probe,
    /// The run ended without a reply; the device was told and service continues.
    Unanswered,
    Device(LinkEvent<()>),
    Session(SessionNotice),
}

/// Serve successive conversations inside `with_device`'s authenticated callback.
/// Take device events once and keep driving process IO before the first wake
/// and between calls. A new peer is created only for an unmuted admitted wake;
/// the conversation retains it across replies until its idle timeout.
///
/// Cancellation must be forwarded here from the host's watch receiver. Active
/// call/media cleanup is awaited before returning to device close/child reaping.
/// A failed conversation terminates this borrower; no replay occurs. The
/// runtime owner replaces the whole authenticated generation.
///
/// # Errors
/// Fixed device/control/media/observer failures and invalid budgets. The caller
/// must await the outer authenticated host even on failure or cancellation.
pub async fn serve<L, R, W, P>(
    client: &mut L,
    io: &mut RpcIo<R, W>,
    options: ServeOptions<'_>,
    peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    observe: impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<(), Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    Box::pin(service(
        client,
        io,
        options,
        peer,
        cancelled,
        observe,
        (None, None, false, None),
    ))
    .await
}

pub(super) async fn service<L, R, W, P>(
    client: &mut L,
    io: &mut RpcIo<R, W>,
    mut options: ServeOptions<'_>,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    mut observe: impl FnMut(ServeNotice) -> Result<(), Error>,
    context: (
        Option<&mut super::voice_tools::Tools>,
        Option<crate::bridge_trace::Trace>,
        bool,
        Option<&super::voice_kill::Termination>,
    ),
) -> Result<(), Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    let (mut tools, trace, wake_earcon, termination) = context;
    super::voice_session::validate(&options.session)?;
    if options.connect_timeout.is_zero() || Instant::now().checked_add(options.connect_timeout).is_none() {
        return Err(Error::Input);
    }
    let mut events = client.events()?;
    // Heap-own the service to keep scoped supervision within the Windows stack.
    let work = Box::pin(async {
        let cancelled = async {
            tokio::select! {
                () = cancelled => (),
                () = super::earcon::failed() => (),
            }
        };
        tokio::pin!(cancelled);
        let mut gate = Gate::new(options.kill_path);
        loop {
            gate.sync(&mut observe)?;
            observe(ServeNotice::Ready)?;
            let Some(microphone) = wait(
                client,
                &mut events,
                io,
                (&mut options.session.muted, &mut gate),
                cancelled.as_mut(),
                &mut observe,
            )
            .await?
            else {
                return Ok(());
            };
            let killed = std::cell::Cell::new(false);
            let result = Box::pin(admitted(
                (&*client, &mut events, microphone),
                io,
                &mut options,
                &mut peer,
                gate.cancel(cancelled.as_mut(), &killed),
                &mut observe,
                (tools.as_deref_mut(), std::time::SystemTime::now, trace.clone()),
            ))
            .await;
            if killed.get()
                && let Some(termination) = termination
            {
                termination.trip();
            }
            if killed.get() && result == Err(Error::Control) {
                // Cancelled negotiation can invalidate IO. Never resume that
                // generation or present readiness after an unsuccessful stop.
                timeout(Duration::from_secs(2), io.flush())
                    .await
                    .map_err(|_| Error::Deadline)?
                    .map_err(bridge_codex::rpc_error)?;
                client.fail_run(echo_link::RunFailure::KillSwitch).await?;
                gate.tripped(&mut observe)?;
            } else if result == Err(Error::NoResponse) {
                unanswered(&*client, &mut observe).await?;
            } else {
                result?;
            }
        }
    });
    super::earcon::scope(client, wake_earcon, work).await
}

/// Only that run failed, and its call is already closed. End the device's run
/// and stay connected for the next wake.
async fn unanswered<L: EchoLink>(
    client: &L,
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<(), Error> {
    client.fail_run(echo_link::RunFailure::Unavailable).await?;
    observe(ServeNotice::Unanswered)
}

async fn admitted<L, R, W, P>(
    mut device: (&L, &mut L::Events, <L::Events as LinkEvents>::Run),
    io: &mut RpcIo<R, W>,
    options: &mut ServeOptions<'_>,
    peer: &mut impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
    context: (
        Option<&mut super::voice_tools::Tools>,
        impl FnOnce() -> std::time::SystemTime,
        Option<crate::bridge_trace::Trace>,
    ),
) -> Result<(), Error>
where
    L: EchoLink,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: MediaPeer,
{
    let (tools, clock, mut trace) = context;
    tokio::pin!(cancelled);
    if let Some(tools) = &tools {
        tools.budget.check_call()?;
    }
    observe(ServeNotice::Listening)?;
    super::earcon::start()?;
    let deadline = Instant::now()
        .checked_add(options.connect_timeout)
        .ok_or(Error::Input)?;
    let mut setup_observer = |notice| observe(ServeNotice::Session(notice));
    let (mut media, instructions) = tokio::select! {
        biased;
        () = cancelled.as_mut() => return Err(Error::Control),
        result = super::earcon::negotiating(device.1, super::earcon::capture_during(&mut device.2, async {
            // One clock sample: the delegated turn and the Live prompt carry the same snapshot.
            let (runner, voice) = super::prompts::snapshot(options.call.instructions, options.call.prompt, &mut trace)?;
            let (now, zone) = (clock(), options.call.time_zone);
            let instructions = (
                super::voice_config::call_instructions(&runner, now, zone)?,
                super::voice_config::call_instructions(&voice, now, zone)?,
            );
            let media = timeout_at(deadline, peer()).await.map_err(|_| Error::Deadline)??;
            Ok::<_, Error>((media, instructions))
        }), &mut setup_observer) => result?,
    };
    super::voice_session::peer_created(&mut media, &mut trace).await?;
    let options_call = options.conversation((&instructions.0, &instructions.1), deadline);
    let mut observer = |notice| {
        if let SessionNotice::Run(Notice::Device(LinkEvent::Mute(value))) = &notice {
            options.session.muted = *value;
        }
        observe(ServeNotice::Session(notice))
    };
    if let Some(tools) = tools {
        return Box::pin(super::voice_tools::conversation(
            device,
            io,
            media,
            (options_call, tools),
            cancelled.as_mut(),
            &mut observer,
        ))
        .await
        .map(|_| ());
    }
    super::voice_session::open_traced(
        device,
        io,
        media,
        (options_call, trace),
        cancelled.as_mut(),
        &mut observer,
    )
    .await
    .map(|_| ())
}

async fn wait<L: EchoLink, R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    client: &L,
    events: &mut L::Events,
    io: &mut RpcIo<R, W>,
    policy: (&mut bool, &mut Gate<'_>),
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
) -> Result<Option<<L::Events as LinkEvents>::Run>, Error> {
    let (muted, gate) = policy;
    tokio::pin!(cancelled);
    let mut poll = tokio::time::interval(Duration::from_millis(20));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Preserve one in-flight probe across device/control activity. Device
    // recovery belongs to the adapter; a failed probe ends this borrower.
    let probe = probe(client);
    tokio::pin!(probe);
    loop {
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            () = crate::bridge_settings::settings_restart::requested() => return Ok(None),
            result = async {
                tokio::select! {
                    _ = poll.tick() => { gate.sync(observe)?; },
                    result = &mut probe => {
                        result?;
                        observe(ServeNotice::Probe)?;
                        probe.set(self::probe(client));
                    },
                    event = io.next_event() => idle_message(io, event.map_err(bridge_codex::rpc_error)?)?,
                    event = events.recv() => match event?.ok_or(Error::Wyoming)? {
                        LinkEvent::Wake(run) => {
                            if gate.sync(observe)? {
                                client.fail_run(echo_link::RunFailure::KillSwitch).await?;
                            } else if !*muted { return Ok(Some(run)); }
                        },
                        LinkEvent::Played | LinkEvent::Other => (),
                        LinkEvent::Mute(value) => {
                            *muted = value;
                            observe(ServeNotice::Device(LinkEvent::Mute(value)))?;
                        }
                        LinkEvent::MicrophoneDropped(count) => observe(ServeNotice::Device(LinkEvent::MicrophoneDropped(count)))?,
                    },
                }
                Ok::<_, Error>(None)
            } => if let Some(run) = result? { return Ok(Some(run)); },
        }
        tokio::task::yield_now().await;
    }
}

fn idle_message<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    message: Message,
) -> Result<(), Error> {
    if let Message::Request { id, .. } = message {
        // No thread/turn has been admitted: never execute an unsolicited tool.
        io.respond(&id, &serde_json::json!({"contentItems":[],"success":false}))
            .map_err(bridge_codex::rpc_error)?;
    }
    Ok(())
}

async fn probe(client: &impl EchoLink) -> Result<(), Error> {
    tokio::time::sleep(Duration::from_secs(1)).await;
    timeout(Duration::from_secs(5), client.probe())
        .await
        .map_err(|_| Error::Deadline)?
        .map_err(Error::from)
}

#[cfg(test)]
#[path = "earcon/voice_admission_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "controls/prompts/admission_tests.rs"]
mod prompt_tests;
