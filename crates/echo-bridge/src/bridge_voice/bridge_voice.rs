//! One run joins Codex control/media, device capture, paced reply and played receipt.
#[path = "earcon/earcon.rs"]
mod earcon;
#[path = "controls/prompts/prompts.rs"]
pub(crate) mod prompts;
#[path = "recovery/retained_link.rs"]
mod retained_link;
#[path = "controls/session_controller.rs"]
mod session_controller;
#[path = "controls/session_tools.rs"]
mod session_tools;
#[path = "controls/tool_ledger.rs"]
mod tool_ledger;
#[path = "controls/tool_policy.rs"]
mod tool_policy;
mod voice_admission;
#[path = "controls/voice_config.rs"]
mod voice_config;
#[path = "controls/voice_entry.rs"]
pub(crate) mod voice_entry;
mod voice_host;
#[path = "controls/voice_kill.rs"]
mod voice_kill;
#[path = "controls/voice_listen.rs"]
mod voice_listen;
#[path = "recovery/voice_recovery.rs"]
mod voice_recovery;
mod voice_runtime;
mod voice_session;
mod voice_source;
#[path = "controls/voice_tools.rs"]
mod voice_tools;
mod voice_view;
pub use voice_admission::{ServeNotice, ServeOptions, serve};
pub use voice_config::Configured;
pub use voice_entry::execute_process;
pub use voice_host::{HostOptions, with_device};
pub use voice_runtime::{RuntimeCompletion, RuntimeOptions, execute, execute_with_settings};
pub use voice_session::{
    ConversationOptions, SessionNotice, SessionOptions, conversation, open_conversation, tool_conversation,
};
#[cfg(test)]
mod voice_tests;

pub use crate::bridge_runtime::bridge_pacing::{BargeTrigger, Endpoint, FirstAudio, ReplyQuality};
use crate::{
    Error,
    bridge_codex::{Event, MediaEvent},
};
use echo_link::{EchoLink, LinkEvent, LinkEvents, MicrophoneRun, PlayOutcome, ReplySink};
use echo_media::PcmFrame;
use std::{future::Future, num::NonZeroUsize, time::Duration};
use tokio::time::{Instant, timeout, timeout_at};
use voice_source::Source;

/// The same run owner accepts production media and memory-only component ports.
pub trait VoicePort {
    /// Bind the run identity, preserving queued tools' original binding.
    fn bind_session(&mut self, _run: Option<std::sync::Arc<()>>) {}
    /// # Errors
    /// The call must be active and the bounded media sender available.
    fn send(&self, frame: PcmFrame) -> Result<(), Error>;
    /// Queue a frame; false retains it with the caller when the sender is full.
    /// # Errors
    /// Same errors as `send`; unbounded ports always accept.
    fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        self.send(frame).map(|()| true)
    }
    /// Resolve once `try_send` can accept a frame. Never borrows the port.
    fn uplink_ready(&self) -> impl Future<Output = ()> + use<Self> {
        std::future::ready(())
    }
    fn next(&mut self) -> impl Future<Output = Result<MediaEvent, Error>>;
    /// Never wait for future audio. EOF/error must be an error, not an empty queue.
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>>;
    /// Mark the interrupted media generation for retirement at replacement.
    fn retire_audio(&mut self) {}
    /// Cumulative RTP packets missing from the call's receive sequence.
    fn rtp_lost(&self) -> u64 {
        0
    }
}

/// No payload-bearing observation implements Debug or Display.
pub enum Notice {
    EarconPlayed(Duration),
    Device(LinkEvent<()>),
    Transcript {
        role: String,
        text: String,
        done: bool,
    },
    /// Audible reply audio reached the Echo's speaker.
    ReplyStarted,
    /// The speaker went quiet: the mouth closes until audio resumes.
    ReplyPaused,
    ReplyPlayed,
    /// Per-reply counters while a persistent run retains capture.
    ReplySettled(Stats),
    SessionTool(session_tools::Receipt),
}

#[derive(Clone, Copy)]
pub struct Options {
    /// None retains the explicit one-reply boundary for existing callers.
    pub listen_timeout: Option<Duration>,
    pub session_max: Duration,
    pub reply_lead_ms: u16,
    pub response_deadline: Instant,
    pub played_timeout: Duration,
    pub muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received_chunks: u64,
    pub dropped_chunks: u64,
    pub microphone_chunks: u64,
    pub microphone_frames: u64,
    pub microphone_tail: usize,
    /// Speech evicted because the media sender stalled past the backlog bound.
    pub uplink_dropped: u64,
    pub reply_received: u64,
    pub reply_sent: u64,
    pub reply_dropped: u64,
    /// The settled reply's pacing and discard facts.
    pub quality: ReplyQuality,
}

/// Run an already connected call without creating a second process or IO reader.
/// Success requires assistant transcript/done, local accepted-media drain, PCM
/// handoff, the device Played event AND the playback operation's Played result.
/// Poll control/capture while pacing, under playback backpressure and awaiting
/// played. A stopped source, empty queue or transcript alone cannot finish.
/// An admitted listen interval retains capture after each reply; ordered user
/// speech or assistant activity starts the next response window. Idle media
/// cannot renew it. None keeps the explicit one-reply lifetime.
///
/// # Errors
/// Tools without a dispatcher, unexpected
/// wake, EOF, cancellation and fixed deadlines fail closed. Error cleanup
/// requests a bounded cut; the host must then await call and device close.
/// Drop/cancellation also requires host cleanup. This owner does not close a
/// healthy call, allowing the host to retain it for the idle conversation.
pub async fn run<L: EchoLink>(
    client: &L,
    events: &mut L::Events,
    mut microphone: <L::Events as LinkEvents>::Run,
    port: &mut impl VoicePort,
    options: Options,
    cancelled: impl Future<Output = ()>,
    mut observe: impl FnMut(Notice) -> Result<(), Error>,
) -> Result<Stats, Error> {
    if options.played_timeout.is_zero()
        || Instant::now() >= options.response_deadline
        || options
            .listen_timeout
            .is_some_and(|value| value.is_zero() || Instant::now().checked_add(value).is_none())
        || Instant::now().checked_add(options.session_max).is_none()
    {
        return Err(Error::Deadline);
    }
    let mut source = Source::new(options.reply_lead_ms, options.muted)?;
    source.session = session_tools::Grants::new(options.listen_timeout, options.session_max);
    port.bind_session(Some(source.session.run.clone()));
    source.controller.admit_response(options.response_deadline)?;
    // A cut replaces playback, while the run still owns pending clock receipts.
    let mut drain = crate::bridge_mcp::bridge_clock::receipt::Context::current()
        .map(|clock| clock.voice())
        .transpose()?;
    tokio::pin!(cancelled);
    // Retain the reply-loop state once per run rather than embedding it in
    // every conversation/admission future above this boundary.
    let result = Box::pin(voice_listen::replies(
        client,
        (events, &mut microphone),
        port,
        (&mut source, &mut drain),
        &options,
        cancelled.as_mut(),
        &mut observe,
    ))
    .await;
    port.bind_session(None);
    result?;
    session_controller::close(
        client,
        (events, &mut microphone),
        port,
        &mut source,
        options.played_timeout,
        cancelled.as_mut(),
        &mut observe,
    )
    .await?;
    if let Some(drain) = drain.take() {
        drain.finish();
    }
    if options.listen_timeout.is_none() {
        observe(Notice::ReplyPlayed)?;
    }
    source.stats.received_chunks = microphone.stats().received;
    source.stats.dropped_chunks = microphone.stats().dropped;
    source.stats.reply_dropped += source.reply.dropped_frames();
    source.stats.quality = source.settle(port.rtp_lost());
    Ok(source.stats)
}

async fn generation<L: EchoLink>(
    client: &L,
    input: (&mut L::Events, &mut <L::Events as LinkEvents>::Run),
    port: &mut impl VoicePort,
    context: (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    options: &Options,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let (events, microphone) = input;
    let (source, drain) = context;
    let (sink, reply) = client.reply_channel(NonZeroUsize::MIN);
    // Keep the producer alive until cut is acknowledged. Closing it first can
    // race the cut command with an ordinary audio-stop in the device owner.
    let mut sink = Some(sink);
    let ceiling = options.response_deadline.saturating_duration_since(Instant::now()) + options.played_timeout;
    let playback_result = std::cell::Cell::new(None);
    let playback_started = std::cell::Cell::new(false);
    let playback = async {
        earcon::idle().await?;
        playback_started.set(true);
        let result = client
            .play_with_timeout(crate::bridge_audio::format(24_000), reply, ceiling)
            .await;
        playback_result.set(Some(result));
        result.map_err(Error::from).and_then(|outcome| match outcome {
            PlayOutcome::Played => Ok(()),
            PlayOutcome::Cut => Err(Error::Wyoming),
        })
    };
    tokio::pin!(playback);
    let producer = async {
        tokio::select! {
            biased;
            () = cancelled => Err(Error::Control),
            result = drive((client, events, microphone), port, &mut sink, (source, drain), options, observe) => result,
        }
    };
    let result = timeout_at(options.response_deadline + options.played_timeout, async {
        tokio::try_join!(&mut playback, producer).map(|_| ())
    })
    .await
    .unwrap_or(Err(Error::Deadline));
    if let Err(error) = result {
        if error == Error::Wyoming {
            voice_source::playback_failure(source.failure, &error, source, sink.is_some(), playback_result.get());
        }
        // The earcon scope still owns the speaker until reply playback starts.
        // Its joined supervisor cuts/collects that operation on borrower exit.
        if !playback_started.get() {
            return Err(error);
        }
        if error == Error::Control {
            voice_source::cut_generation(
                client,
                playback.as_mut(),
                &playback_result,
                options.played_timeout,
                source,
                sink.is_some(),
            )
            .await?;
        } else {
            let _ = timeout(Duration::from_secs(1), client.cut_with_stats()).await;
        }
        return Err(error);
    }
    Ok(())
}

async fn drive<L: EchoLink>(
    link: (&L, &mut L::Events, &mut <L::Events as LinkEvents>::Run),
    port: &mut impl VoicePort,
    sink: &mut Option<impl ReplySink>,
    context: (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    options: &Options,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let (client, events, microphone) = link;
    let (source, drain) = context;
    let mut pending = None;
    let mut deadline = options.response_deadline;
    loop {
        if source.controller.cutting() {
            return Err(Error::Control);
        }
        voice_source::think(client, source).await;
        let barge = source.barge_due();
        if source.reply.drained() && pending.is_none() && sink.is_some() {
            drop(sink.take());
            deadline = Instant::now() + options.played_timeout;
        }
        tokio::select! {
            // Timeouts cannot be starved by a continuously ready peer.
            biased;
            duration = earcon::completed() => observe(Notice::EarconPlayed(duration))?,
            () = tokio::time::sleep_until(deadline) => {
                if sink.is_none() {
                    return Err(Error::Deadline);
                }
                if !source.delegation.held() {
                    // The device is healthy; only this run goes unanswered.
                    return Err(Error::NoResponse);
                }
                // The filler played; a delegated answer missing the response
                // deadline is dropped instead of failing the whole run.
                source.delegation.abandon();
                source.reply.finish()?;
                deadline = Instant::now() + options.played_timeout;
            },
            () = tokio::time::sleep_until(barge.unwrap_or(deadline)), if barge.is_some() => source.interrupt(port)?,
            () = tokio::time::sleep_until(source.mouth.due().unwrap_or(deadline)), if source.mouth.due().is_some() => {
                if let Some(open) = source.mouth.poll(Instant::now()) {
                    observe(if open { Notice::ReplyStarted } else { Notice::ReplyPaused })?;
                }
            },
            result = step(events, microphone, port, sink.as_ref(), source, &mut pending, observe) => result?,
            () = async {
                match drain {
                    Some(drain) => drain.acknowledge().await,
                    None => std::future::pending().await,
                }
            }, if pending.is_none() => {},
        }
        if source
            .controller
            .ready_to_close(sink.is_none(), source.mouth.due().is_none())
        {
            return Ok(());
        }
    }
}

async fn step<E: LinkEvents>(
    events: &mut E,
    microphone: &mut E::Run,
    port: &mut impl VoicePort,
    sink: Option<&impl ReplySink>,
    source: &mut Source,
    pending: &mut Option<Vec<u8>>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let uplink = port.uplink_ready();
    tokio::select! {
        // Setup-time speech arrives as one burst; send what the peer refused.
        () = uplink, if source.uplink_pending() => source.flush_uplink(port),
        event = events.recv() => match event {
            Ok(Some(event)) => device(&event, sink.is_none(), source, observe),
            Ok(None) => { source.failure = "events_eof"; Err(Error::Wyoming) },
            Err(error) => { source.failure = "events_recv"; Err(error.into()) },
        },
        frame = earcon::microphone(microphone), if !source.controller.microphone_finished() => {
            let frame = frame.map_err(|error| { source.failure = "microphone_recv"; Error::from(error) })?;
            if source.capture(frame, port)? {
                observe(Notice::Device(LinkEvent::MicrophoneDropped(1)))?;
            }
            Ok(())
        },
        event = port.next() => media(event?, port, source, observe).await,
        frame = source.reply.next_or_fade(), if pending.is_none() && source.playable() => {
            *pending = frame;
            Ok(())
        },
        result = forward(sink, pending.as_deref()), if pending.is_some() => {
            result.inspect_err(|_| { source.failure = "reply_send"; })?;
            if let Some(frame) = pending.take() {
                source.mouth.wrote(&frame, Instant::now());
                source.meter.wrote(Instant::now());
                if let Some(clock) = crate::bridge_mcp::bridge_clock::receipt::Context::current() {
                    clock.submitted("reply")?;
                }
            }
            source.stats.reply_sent += 1;
            Ok(())
        },
    }
}

async fn forward(sink: Option<&impl ReplySink>, pending: Option<&[u8]>) -> Result<(), Error> {
    let sink = sink.ok_or(Error::Wyoming)?;
    let frame = pending.ok_or(Error::Audio)?;
    earcon::preempt();
    earcon::idle().await?;
    // Retain the single frame across cancelled bounded sends.
    sink.send(frame.to_vec()).await.map_err(Error::from)
}

fn device<R>(
    event: &LinkEvent<R>,
    stopped: bool,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    if earcon::receipt(event) {
        return Ok(());
    }
    let event = match event {
        LinkEvent::Played => {
            match source.controller.played(stopped) {
                Ok(false) => return Ok(()),
                Ok(true) => {}
                Err(error) => {
                    source.failure = "premature_played";
                    return Err(error);
                }
            }
            LinkEvent::Played
        }
        LinkEvent::Mute(muted) => {
            // Repeated snapshots must not erase partial PCM or filter history.
            if source.muted != *muted {
                source.mute_uplink();
            }
            source.muted = *muted;
            observe(Notice::Device(LinkEvent::Mute(*muted)))?;
            return Ok(());
        }
        LinkEvent::MicrophoneDropped(count) => LinkEvent::MicrophoneDropped(*count),
        LinkEvent::Other => LinkEvent::Other,
        LinkEvent::Wake(_) => {
            source.failure = "unexpected_wake";
            return Err(Error::Wyoming);
        }
    };
    observe(Notice::Device(event))
}

async fn media(
    event: MediaEvent,
    port: &mut impl VoicePort,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    match event {
        MediaEvent::Audio(frame) => source.audio(&frame),
        MediaEvent::Control(Event::ReplyDone { text }) => {
            if source.controller.retired() {
                return Ok(());
            }
            observe(Notice::Transcript {
                role: "assistant".into(),
                text,
                done: true,
            })?;
            if !source.delegation.answered() {
                return Ok(());
            }
            if !source.reply.finished() {
                while let Some(frame) = port.ready_audio().await? {
                    source.audio(&frame)?;
                    tokio::task::yield_now().await;
                }
                source.reply.finish()?;
            }
            Ok(())
        }
        MediaEvent::Control(Event::Transcript { role, text, done }) => {
            if role == "user" {
                source.meter.endpoint(Instant::now(), Endpoint::UserTranscript);
                source.heard_user(BargeTrigger::UserTranscript);
            } else if role == "assistant" && !done {
                source.controller.reply_started();
                source.delegation.spoke();
            }
            observe(Notice::Transcript { role, text, done })
        }
        MediaEvent::Control(Event::TurnStarted { .. }) => {
            source.meter.endpoint(Instant::now(), Endpoint::DelegatedTurn);
            source.delegation.started();
            Ok(())
        }
        MediaEvent::Control(Event::TurnCompleted { .. }) => {
            source.delegation.completed();
            Ok(())
        }
        MediaEvent::Control(Event::SpeechStarted) => {
            source.heard_user(BargeTrigger::SpeechStarted);
            Ok(())
        }
        MediaEvent::Control(Event::Session(request)) => {
            session_tools::answer(request, &mut source.session, &mut source.controller, observe)
        }
        MediaEvent::Control(Event::Other) => Ok(()),
        MediaEvent::Control(Event::Closed | Event::Tool { .. }) => Err(Error::Unavailable),
    }
}
