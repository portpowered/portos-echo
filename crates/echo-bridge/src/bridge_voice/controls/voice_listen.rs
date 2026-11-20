//! Keep capture and the run owned while a settled reply waits for activity.
use super::{Error, Event, MediaEvent, Notice, Options, Source, VoicePort};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use std::{future::Future, time::Duration};

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
            observe(Notice::ReplyPaused)?;
            if options.listen_timeout.is_none() {
                generation_options.response_deadline = source.controller.replacement_deadline()?;
                continue;
            }
            // The model yielded and may say nothing more: the cut reply
            // settles and the run listens, with no playback left open.
            source.controller.cut_played();
        } else {
            result?;
        }
        if options.listen_timeout.is_none() && !source.delegation.in_flight() {
            return Ok(());
        }
        source.controller.listen_after_reply(window(source, options)?)?;
        // Waiting granted during the reply outlives it, within the run's cap.
        if let Some(granted) = source.session.granted() {
            source.controller.extend_idle(granted);
        }
        if options.listen_timeout.is_some() {
            settled(source, port, &mut previous, observe)?;
        }
        // A requested end closes right after this reply settles; no idle wait.
        if source.session.ending() {
            return Ok(());
        }
        // A dispatch that outlives its spoken acknowledgement shows as thinking.
        think(client, source, observe).await?;
        if !listen(
            (events, microphone),
            port,
            (source, drain),
            options.listen_timeout.is_some(),
            cancelled.as_mut(),
            observe,
        )
        .await?
        {
            return Ok(());
        }
        generation_options.response_deadline = source.controller.replacement_deadline()?;
    }
}

pub(super) async fn think<L: EchoLink>(
    client: &L,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    if let Some(on) = source.thinking_change() {
        if on {
            // The screen holds this state through queued audio. The mouth's
            // playout edge replaces it once the reply reaches the speaker.
            observe(Notice::Thinking)?;
        }
        // The device indication remains best effort.
        let _ = tokio::time::timeout(Duration::from_secs(1), client.thinking(on)).await;
    }
    Ok(())
}

fn settled(
    source: &mut Source,
    port: &impl VoicePort,
    previous: &mut super::Stats,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut stats = source.stats.clone();
    stats.reply_dropped += source.reply.dropped_frames();
    let totals = stats.clone();
    stats.reply_received -= previous.reply_received;
    stats.reply_sent -= previous.reply_sent;
    stats.reply_dropped -= previous.reply_dropped;
    stats.quality = source.settle(port.rtp_lost());
    *previous = totals;
    observe(Notice::ReplySettled(stats))?;
    observe(Notice::ReplyPlayed)?;
    Ok(())
}

async fn listen<E: LinkEvents>(
    input: (&mut E, &mut E::Run),
    port: &mut impl VoicePort,
    context: (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    followup: bool,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    let (events, microphone) = input;
    let (source, drain) = context;
    tokio::pin!(cancelled);
    // A dispatch still in flight holds the run open past idle. It started
    // before this wait, so its own task deadline ends it inside this window.
    let hold = source.controller.replacement_deadline().ok();
    loop {
        // A granted extension moves the deadline; a requested end closes now.
        let idle = source.controller.idle_deadline().ok_or(Error::Input)?;
        let deadline = match hold {
            Some(hold) if source.delegation.in_flight() => idle.max(hold),
            _ => idle,
        };
        if source.session.ending() || (!followup && !source.delegation.in_flight()) {
            return Ok(false);
        }
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            // Playback has settled before entering listen. A running delegated
            // turn still owns this reply; finish it before admitting cleanup.
            () = crate::bridge_settings::settings_restart::requested(), if !source.delegation.in_flight() => return Ok(false),
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
        // A device that already ended capture keeps the run until idle or
        // explicit end; its settled EOF is never polled again.
        frame = super::earcon::microphone(microphone), if !source.controller.microphone_finished() => {
            if source.capture(frame?, port)? {
                observe(Notice::Device(LinkEvent::MicrophoneDropped(1)))?;
            }
        },
        event = port.next() => {
            let event = event?;
            if activity(&event) {
                // After a barge-in only the model's answer opens a reply.
                // More of the user's speech keeps the run listening for it.
                if source.stale() && !answer(&event) {
                    source.renew_idle();
                    super::media(event, port, source, observe).await?;
                    return Ok(false);
                }
                source.resume_reply()?;
                super::media(event, port, source, observe).await?;
                return Ok(true);
            }
            match event {
                MediaEvent::Control(Event::Session(request)) => {
                    super::session_tools::answer(request, &mut source.session, &mut source.controller, observe)?;
                }
                MediaEvent::Control(Event::Closed | Event::Tool { .. }) => return Err(Error::Unavailable),
                MediaEvent::Control(Event::TurnCompleted { .. }) => source.dispatch_completed(),
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

/// The listen window. Off suppresses new follow-up admission, not work
/// already admitted.
fn window(source: &Source, options: &Options) -> Result<std::time::Duration, Error> {
    if let Some(timeout) = options.listen_timeout {
        return Ok(timeout);
    }
    let deadline = source.controller.replacement_deadline()?;
    Ok(deadline.saturating_duration_since(tokio::time::Instant::now()))
}

/// Activity that is the model's own: a delegated turn or assistant speech.
fn answer(event: &MediaEvent) -> bool {
    matches!(event, MediaEvent::Control(Event::TurnStarted { .. }))
        || matches!(event, MediaEvent::Control(Event::Transcript { role, .. }) if role == "assistant")
}

#[cfg(test)]
#[path = "tests/voice_listen_tests.rs"]
mod tests;
