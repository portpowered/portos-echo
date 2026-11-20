//! Keep capture and the run owned while a settled reply waits for activity.
use super::{Error, Event, MediaEvent, Notice, Options, Source, VoicePort, session_tools::Goodbye};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use std::{future::Future, time::Duration};
use tokio::time::Instant;

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
            if !settle_cut(source, port, options.listen_timeout.is_some(), observe)? {
                generation_options.response_deadline = source.controller.replacement_deadline()?;
                continue;
            }
        } else {
            result?;
        }
        if options.listen_timeout.is_some() {
            settled(source, port, &mut previous, observe)?;
        }
        if !after_reply(
            client,
            (events, microphone),
            port,
            (source, drain),
            &generation_options,
            cancelled.as_mut(),
            observe,
        )
        .await?
        {
            break;
        }
        generation_options.response_deadline = source.controller.replacement_deadline()?;
    }
    if source.session.ending() {
        tokio::time::timeout(options.played_timeout, port.flush_session_answer())
            .await
            .map_err(|_| Error::Deadline)??;
    }
    Ok(())
}

fn settle_cut(
    source: &mut Source,
    port: &mut impl VoicePort,
    followup: bool,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    // An interrupted farewell cannot end the replacement reply's run.
    // A separate delegated end stays armed.
    let held = source.clear_goodbye_hold();
    let native = source.session.goodbye.take().is_some();
    if (held || native) && !source.session.ending() {
        retire_goodbye(source, port);
    }
    source.controller.cut_settled();
    observe(Notice::ReplyPaused)?;
    if followup {
        // The cut reply settles with no playback left open, even if the
        // model says nothing more.
        source.controller.cut_played();
    }
    Ok(followup)
}

async fn after_reply<L: EchoLink>(
    client: &L,
    (events, microphone): (&mut L::Events, &mut <L::Events as LinkEvents>::Run),
    port: &mut impl VoicePort,
    (source, drain): (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    options: &Options,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
    tokio::pin!(cancelled);
    if waiting_for_goodbye(source) {
        wait_goodbye(
            (events, microphone),
            port,
            (source, drain),
            options.response_deadline,
            cancelled.as_mut(),
            observe,
        )
        .await?;
    }
    // A granted end closes at the settled reply boundary.
    if source.session.ending() {
        return Ok(false);
    }
    if source.session.goodbye.take().is_some() {
        retire_goodbye(source, port);
    }
    let followup = options.listen_timeout.is_some();
    if !followup && !source.delegation.in_flight() {
        return Ok(false);
    }
    source.controller.listen_after_reply(window(source, options)?)?;
    if let Some(granted) = source.session.granted() {
        source.controller.extend_idle(granted);
    }
    think(client, source, observe).await?;
    listen(
        (events, microphone),
        port,
        (source, drain),
        followup,
        cancelled.as_mut(),
        observe,
    )
    .await
}

pub(super) fn goodbye_handoff(source: &mut Source) {
    if !source.stale() && source.session.listen().is_some() && source.session.goodbye.is_none() {
        source.clear_goodbye_hold();
        source.session.goodbye = Some(Goodbye::Pending);
        // The native turn can start before its hand-off notification arrives.
        // Its only audio is the farewell already owned by this reply.
        source.delegation = super::voice_source::Delegation::default();
    }
}

pub(super) fn complete_turn(source: &mut Source) {
    match source.session.goodbye {
        Some(Goodbye::Pending) => source.session.goodbye = Some(Goodbye::Finished),
        Some(_) => {}
        None => source.delegation.completed(),
    }
}

fn retire_goodbye(source: &mut Source, port: &mut impl VoicePort) {
    source.session.run = std::sync::Arc::new(());
    port.bind_session(Some(source.session.run.clone()));
}

fn waiting_for_goodbye(source: &Source) -> bool {
    (source.holds_goodbye() || source.session.goodbye == Some(Goodbye::Pending)) && !source.session.ending()
}

async fn wait_goodbye<E: LinkEvents>(
    (events, microphone): (&mut E, &mut E::Run),
    port: &mut impl VoicePort,
    (source, drain): (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    tokio::pin!(cancelled);
    while waiting_for_goodbye(source) {
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            () = tokio::time::sleep_until(deadline) => break,
            result = goodbye_step(events, microphone, port, source, observe) => result?,
            () = clock_step(drain) => {},
        }
        tokio::task::yield_now().await;
    }
    if !source.session.ending() {
        // Retire this turn's run token before normal listening resumes.
        source.clear_goodbye_hold();
        source.session.goodbye = None;
        retire_goodbye(source, port);
    }
    Ok(())
}

async fn goodbye_step<E: LinkEvents>(
    events: &mut E,
    microphone: &mut E::Run,
    port: &mut impl VoicePort,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    tokio::select! {
        event = events.recv() => super::device(&event?.ok_or(Error::Wyoming)?, true, source, observe),
        frame = super::earcon::microphone(microphone), if !source.controller.microphone_finished() => {
            if frame?.is_none() {
                source.controller.microphone_ended();
            }
            Ok(())
        },
        event = port.next() => match event? {
            MediaEvent::Control(Event::Session(request)) => {
                super::session_tools::answer(request, &mut source.session, &mut source.controller, observe)
            }
            MediaEvent::Control(Event::Closed | Event::Tool { .. }) => Err(Error::Unavailable),
            MediaEvent::Control(Event::GoodbyeHandoff) => { goodbye_handoff(source); Ok(()) },
            MediaEvent::Control(Event::TurnCompleted { .. }) => { complete_turn(source); Ok(()) },
            _ => Ok(()),
        },
    }
}

async fn clock_step(drain: &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>) {
    match drain {
        Some(drain) => drain.acknowledge().await,
        None => std::future::pending().await,
    }
}

/// A silent gap in a running turn is Thinking, even with more playout queued.
pub(super) fn playout_notice(mouth: &mut super::voice_source::Mouth, delegating: bool) -> Option<Notice> {
    Some(match (mouth.poll(tokio::time::Instant::now())?, delegating) {
        (true, _) => Notice::ReplyStarted,
        (false, true) => Notice::Thinking,
        (false, false) => Notice::ReplyPaused,
    })
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

pub(super) async fn finish_reply(
    text: String,
    port: &mut impl VoicePort,
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    if source.stale() {
        return Ok(());
    }
    observe(Notice::Transcript {
        role: "assistant".into(),
        text,
        done: true,
    })?;
    if source.session.goodbye.is_none() && !source.delegation.answered() {
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
    let mut turn_started = false;
    loop {
        // A granted extension moves the deadline; a requested end closes now.
        let idle = source.controller.idle_deadline().ok_or(Error::Input)?;
        if source.session.goodbye == Some(Goodbye::Pending) && !source.session.ending() {
            wait_goodbye(
                (events, microphone),
                port,
                (source, drain),
                idle.max(hold.unwrap_or(idle)),
                cancelled.as_mut(),
                observe,
            )
            .await?;
        }
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
            result = listening_step((events, microphone), port, source, &mut turn_started, observe) => {
                if result? { return Ok(true); }
            },
            // Keep the same clock owner across replies. Acknowledge only after
            // due idle expiry, cancellation and ready capture/control work.
            () = clock_step(drain) => {},
        }
        tokio::task::yield_now().await;
    }
}

async fn listening_step<E: LinkEvents>(
    (events, microphone): (&mut E, &mut E::Run),
    port: &mut impl VoicePort,
    source: &mut Source,
    turn_started: &mut bool,
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
            // The runner can start before the native hand-off identifies the
            // request. Keep Played until the hand-off or speech opens a reply.
            if matches!(event, MediaEvent::Control(Event::GoodbyeHandoff)) {
                *turn_started = false;
                super::media(event, port, source, observe).await?;
                return Ok(false);
            }
            if !source.stale() && matches!(event, MediaEvent::Control(Event::TurnStarted { .. })) {
                if !source.delegation.in_flight() {
                    source.delegation = super::voice_source::Delegation::default();
                }
                *turn_started = true;
                super::media(event, port, source, observe).await?;
                return Ok(false);
            }
            if activity(&event) {
                // After a barge-in only the model's answer opens a reply.
                // More of the user's speech keeps the run listening for it.
                if source.stale() && !answer(&event) {
                    source.renew_idle();
                    super::media(event, port, source, observe).await?;
                    return Ok(false);
                }
                // An already-started follow-up still owes its acknowledgement
                // and delegated answer in this reply.
                let delegation = (*turn_started && source.delegation.in_flight())
                    .then(|| std::mem::take(&mut source.delegation));
                source.resume_reply()?;
                if let Some(delegation) = delegation {
                    source.delegation = delegation;
                }
                super::media(event, port, source, observe).await?;
                return Ok(true);
            }
            match event {
                MediaEvent::Control(Event::Session(request)) => {
                    super::session_tools::answer(request, &mut source.session, &mut source.controller, observe)?;
                }
                MediaEvent::Control(Event::Closed | Event::Tool { .. }) => return Err(Error::Unavailable),
                MediaEvent::Control(Event::TurnCompleted { .. }) => dispatch_completed(source),
                // Continuous RTP and stale done frames cannot renew listening.
                MediaEvent::Control(_) | MediaEvent::Audio(_) => (),
            }
        },
    }
    Ok(false)
}

/// A dispatch that finished while listening gives its answer a fresh
/// listen window, since the hold on idle ends with it.
fn dispatch_completed(source: &mut Source) {
    let waited = source.delegation.in_flight();
    source.delegation.completed();
    if waited && !source.delegation.in_flight() {
        source.renew_idle();
    }
}

fn activity(event: &MediaEvent) -> bool {
    matches!(
        event,
        MediaEvent::Control(Event::SpeechStarted | Event::ReplyHandoff | Event::TurnStarted { .. })
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
    matches!(
        event,
        MediaEvent::Control(Event::ReplyHandoff | Event::TurnStarted { .. })
    ) || matches!(event, MediaEvent::Control(Event::Transcript { role, .. }) if role == "assistant")
}

#[cfg(test)]
#[path = "tests/voice_listen_tests.rs"]
mod tests;
