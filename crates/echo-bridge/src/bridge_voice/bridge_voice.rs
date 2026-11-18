//! One run joins Codex control/media, device capture, paced reply and played receipt.
#[path = "earcon/earcon.rs"]
mod earcon;
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
mod voice_runtime;
mod voice_session;
mod voice_source;
#[path = "controls/voice_tools.rs"]
mod voice_tools;
mod voice_view;
pub use voice_admission::{ServeNotice, ServeOptions, serve};
pub use voice_config::Configured;
pub use voice_host::{HostOptions, with_device};
pub use voice_runtime::{RuntimeOptions, execute};
pub use voice_session::{
    ConversationOptions, SessionNotice, SessionOptions, conversation, open_conversation, tool_conversation,
};
#[cfg(test)]
mod voice_tests;

use crate::{
    Error,
    bridge_codex::{Event, MediaCall, MediaEvent},
};
use echo_link::{EchoLink, LinkEvent, LinkEvents, MicrophoneRun, PlayOutcome, ReplySink};
use echo_media::{MediaPeer, PcmFrame};
use std::{future::Future, num::NonZeroUsize, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout, timeout_at},
};
use voice_source::{ReplyState, Source};

/// The same run owner accepts production media and memory-only component ports.
pub trait VoicePort {
    /// # Errors
    /// The call must be active and the bounded media sender available.
    fn send(&self, frame: PcmFrame) -> Result<(), Error>;
    fn next(&mut self) -> impl Future<Output = Result<MediaEvent, Error>>;
    /// Never wait for future audio. EOF/error must be an error, not an empty queue.
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>>;
    /// Cancel the owned delegated turn once; ports without turns do nothing.
    /// # Errors
    /// Reports the existing bounded interrupt admission failure.
    fn interrupt_turn(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer> VoicePort for MediaCall<'_, R, W, P> {
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        Self::send(self, frame)
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        Self::next(self).await
    }
    async fn ready_audio(&mut self) -> Result<Option<PcmFrame>, Error> {
        Self::ready_audio(self).await
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
}

pub struct Options {
    pub reply_lead_ms: u16,
    pub response_deadline: Instant,
    pub played_timeout: Duration,
    pub muted: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received_chunks: u64,
    pub dropped_chunks: u64,
    pub microphone_chunks: u64,
    pub microphone_frames: u64,
    pub microphone_tail: usize,
    pub reply_received: u64,
    pub reply_sent: u64,
    pub reply_dropped: u64,
}

/// Run an already connected call without creating a second process or IO reader.
/// Success requires assistant transcript/done, local accepted-media drain, PCM
/// handoff, the device Played event AND the playback operation's Played result.
/// Poll control/capture while pacing, under playback backpressure and awaiting
/// played. A stopped source, empty queue or transcript alone cannot finish.
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
    if options.played_timeout.is_zero() || Instant::now() >= options.response_deadline {
        return Err(Error::Deadline);
    }
    let mut source = Source::new(options.reply_lead_ms, options.muted)?;
    // A cut replaces playback, while the run still owns pending clock receipts.
    let mut drain = crate::bridge_mcp::bridge_clock::receipt::Context::current()
        .map(|clock| clock.voice())
        .transpose()?;
    tokio::pin!(cancelled);
    loop {
        let result = generation(
            client,
            (events, &mut microphone),
            port,
            (&mut source, &mut drain),
            &options,
            cancelled.as_mut(),
            &mut observe,
        )
        .await;
        if result == Err(Error::Control) && source.state == ReplyState::Cutting {
            source.state = ReplyState::Retired;
            source.cut_receipts += usize::from(!source.played);
            source.played = false;
            observe(Notice::ReplyPaused)?;
        } else {
            result?;
            break;
        }
    }
    if let Some(drain) = drain.take() {
        drain.finish();
    }
    observe(Notice::ReplyPlayed)?;
    source.stats.received_chunks = microphone.stats().received;
    source.stats.dropped_chunks = microphone.stats().dropped;
    source.stats.reply_dropped += source.reply.dropped_frames();
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
            result = drive(events, microphone, port, sink, (source, drain), options, observe) => result,
        }
    };
    let result = timeout_at(options.response_deadline + options.played_timeout, async {
        tokio::try_join!(&mut playback, producer).map(|_| ())
    })
    .await
    .unwrap_or(Err(Error::Deadline));
    if let Err(error) = result {
        // The earcon scope still owns the speaker until reply playback starts.
        // Its joined supervisor cuts/collects that operation on borrower exit.
        if !playback_started.get() {
            return Err(error);
        }
        let cut = timeout(Duration::from_secs(1), client.cut_with_stats()).await;
        if error == Error::Control {
            cut.map_err(|_| Error::Deadline)??;
            voice_source::settle_cut(playback.as_mut(), &playback_result, options.played_timeout).await?;
        }
        return Err(error);
    }
    Ok(())
}

async fn drive<E: LinkEvents>(
    events: &mut E,
    microphone: &mut E::Run,
    port: &mut impl VoicePort,
    sink: impl ReplySink,
    context: (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    options: &Options,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let (source, drain) = context;
    let mut sink = Some(sink);
    let mut pending = None;
    let mut deadline = options.response_deadline;
    loop {
        if source.state == ReplyState::Cutting {
            return Err(Error::Control);
        }
        if source.reply.drained() && pending.is_none() && sink.is_some() {
            drop(sink.take());
            deadline = Instant::now() + options.played_timeout;
        }
        tokio::select! {
            // Timeouts cannot be starved by a continuously ready peer.
            biased;
            duration = earcon::completed() => observe(Notice::EarconPlayed(duration))?,
            () = tokio::time::sleep_until(deadline) => {
                if sink.is_none() || !source.delegation.held() {
                    return Err(Error::Deadline);
                }
                // The filler played; a delegated answer missing the response
                // deadline is dropped instead of failing the whole run.
                source.delegation.abandon();
                source.reply.finish()?;
                deadline = Instant::now() + options.played_timeout;
            },
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
        if source.state != ReplyState::Cutting
            && source.played
            && source.microphone_finished
            && sink.is_none()
            && source.mouth.due().is_none()
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
    tokio::select! {
        event = events.recv() => device(&event?.ok_or(Error::Wyoming)?, sink.is_none(), source, observe),
        frame = earcon::microphone(microphone), if !source.microphone_finished => {
            if source.capture(frame?, port)? {
                observe(Notice::Device(LinkEvent::MicrophoneDropped(1)))?;
            }
            Ok(())
        },
        event = port.next() => media(event?, port, source, observe).await,
        frame = source.reply.next_frame(), if pending.is_none() && source.reply.has_frames() => {
            *pending = frame;
            Ok(())
        },
        result = forward(sink, pending.as_deref()), if pending.is_some() => {
            result?;
            if let Some(frame) = pending.take() {
                source.mouth.wrote(&frame, Instant::now());
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
            if source.cut_receipts > 0 {
                source.cut_receipts -= 1;
                return Ok(());
            }
            if !stopped {
                return Err(Error::Wyoming);
            }
            source.played = true;
            LinkEvent::Played
        }
        LinkEvent::Mute(muted) => {
            // Repeated snapshots must not erase partial PCM or filter history.
            if source.muted != *muted {
                source.microphone = None;
            }
            source.muted = *muted;
            observe(Notice::Device(LinkEvent::Mute(*muted)))?;
            return Ok(());
        }
        LinkEvent::MicrophoneDropped(count) => LinkEvent::MicrophoneDropped(*count),
        LinkEvent::Other => LinkEvent::Other,
        LinkEvent::Wake(_) => return Err(Error::Wyoming),
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
            if source.state == ReplyState::Retired {
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
                source.interrupt(port)?;
            } else if role == "assistant" && !done {
                source.state = ReplyState::Speaking;
                source.delegation.spoke();
            }
            observe(Notice::Transcript { role, text, done })
        }
        MediaEvent::Control(Event::TurnStarted { .. }) => {
            source.delegation.started();
            Ok(())
        }
        MediaEvent::Control(Event::TurnCompleted { .. }) => {
            source.delegation.completed();
            Ok(())
        }
        MediaEvent::Control(Event::SpeechStarted) => source.interrupt(port),
        MediaEvent::Control(Event::Other) => Ok(()),
        MediaEvent::Control(Event::Closed | Event::Tool { .. }) => Err(Error::Unavailable),
    }
}
