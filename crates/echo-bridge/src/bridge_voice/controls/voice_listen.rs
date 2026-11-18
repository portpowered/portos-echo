//! Keep capture and the run owned while a settled reply waits for activity.
use super::{Error, Event, MediaEvent, Notice, Options, Source, VoicePort};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use std::future::Future;

pub(super) async fn replies<L: EchoLink>(
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
    let mut generation_options = *options;
    let mut previous = super::Stats::default();
    tokio::pin!(cancelled);
    loop {
        let result = super::generation(
            client,
            (events, microphone),
            port,
            (source, drain),
            &generation_options,
            cancelled.as_mut(),
            observe,
        )
        .await;
        if result == Err(Error::Control) && source.controller.cutting() {
            source.controller.cut_settled();
            generation_options.response_deadline = source.controller.replacement_deadline()?;
            observe(Notice::ReplyPaused)?;
            continue;
        }
        result?;
        let Some(timeout) = options.listen_timeout else {
            return Ok(());
        };
        source.controller.listen_after_reply(timeout)?;
        let mut stats = source.stats.clone();
        stats.reply_dropped += source.reply.dropped_frames();
        let totals = stats.clone();
        stats.reply_received -= previous.reply_received;
        stats.reply_sent -= previous.reply_sent;
        stats.reply_dropped -= previous.reply_dropped;
        previous = totals;
        observe(Notice::ReplySettled(stats))?;
        observe(Notice::ReplyPlayed)?;
        if !listen((events, microphone), port, (source, drain), cancelled.as_mut(), observe).await? {
            return Ok(());
        }
        generation_options.response_deadline = source.controller.replacement_deadline()?;
    }
}

async fn listen<E: LinkEvents>(
    input: (&mut E, &mut E::Run),
    port: &mut impl VoicePort,
    context: (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    let (events, microphone) = input;
    let (source, drain) = context;
    let deadline = source.controller.idle_deadline().ok_or(Error::Input)?;
    tokio::pin!(cancelled);
    loop {
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            () = tokio::time::sleep_until(deadline) => return Ok(false),
            result = listening_step(events, microphone, port, source, observe) => {
                if result? { return Ok(true); }
            },
            // Keep the same clock owner across replies. Acknowledge only after
            // due idle expiry, cancellation and ready capture/control work.
            () = async {
                match drain {
                    Some(drain) => drain.acknowledge().await,
                    None => std::future::pending().await,
                }
            } => {},
        }
        tokio::task::yield_now().await;
    }
}

async fn listening_step<E: LinkEvents>(
    events: &mut E,
    microphone: &mut E::Run,
    port: &mut impl VoicePort,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    tokio::select! {
        event = events.recv() => super::device(&event?.ok_or(Error::Wyoming)?, true, source, observe)?,
        frame = super::earcon::microphone(microphone) => {
            let frame = frame?.ok_or(Error::Wyoming)?;
            if source.capture(Some(frame), port)? {
                observe(Notice::Device(LinkEvent::MicrophoneDropped(1)))?;
            }
        },
        event = port.next() => {
            let event = event?;
            if activity(&event) {
                source.resume_reply()?;
                super::media(event, port, source, observe).await?;
                return Ok(true);
            }
            match event {
                MediaEvent::Control(Event::Closed | Event::Tool { .. }) => return Err(Error::Unavailable),
                // Continuous RTP and stale done frames cannot renew listening.
                MediaEvent::Control(_) | MediaEvent::Audio(_) => (),
            }
        },
    }
    Ok(false)
}

fn activity(event: &MediaEvent) -> bool {
    matches!(
        event,
        MediaEvent::Control(Event::SpeechStarted | Event::TurnStarted { .. })
    ) || matches!(event, MediaEvent::Control(Event::Transcript { role, done, .. })
            if role == "user" || (role == "assistant" && !done))
}

#[cfg(test)]
#[path = "tests/voice_listen_tests.rs"]
mod tests;
