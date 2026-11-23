//! Keep capture and the run owned while a settled reply waits for activity.
use super::{Error, Event, MediaEvent, Notice, Options, Source, VoicePort};
use echo_link::{EchoLink, LinkEvent, LinkEvents, ReplySink};
use std::{future::Future, time::Duration};
use tokio::time::Instant;

pub(super) async fn until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(super) fn remote_cut(port: &mut impl VoicePort, source: &mut Source) -> Result<(), Error> {
    source.session.request_end();
    source.controller.cut();
    port.retire_audio();
    Err(Error::Control)
}

pub(super) async fn forward(sink: Option<&impl ReplySink>, pending: Option<&[u8]>) -> Result<(), Error> {
    let sink = sink.ok_or(Error::Wyoming)?;
    let frame = pending.ok_or(Error::Audio)?;
    super::earcon::preempt();
    super::earcon::idle().await?;
    // Retain the single frame across cancelled bounded sends.
    sink.send(frame.to_vec()).await.map_err(Error::from)
}

/// Every reply of one run; the run's open speech latch is reported at its end.
pub(super) async fn replies<L: EchoLink>(
    client: &L,
    input: (&mut L::Events, &mut <L::Events as LinkEvents>::Run),
    port: &mut impl VoicePort,
    (source, drain): (
        &mut Source,
        &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>,
    ),
    options: &Options,
    cancelled: impl Future<Output = ()>,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let result = reply_loop(client, input, port, (&mut *source, drain), options, cancelled, observe).await;
    let marks = super::voice_wait::run_marks::run_ended(source, observe);
    result.and(marks)
}

async fn reply_loop<L: EchoLink>(
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
    source.answer_due = options.response_deadline;
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
            // An applied end cut the reply: it settles and the run closes.
            let ending = source.session.ending();
            if !settle_cut(source, options.listen_timeout.is_some() || ending, observe)? {
                generation_options.response_deadline = source.controller.replacement_deadline()?;
                source.answer_due = generation_options.response_deadline;
                continue;
            }
        } else {
            result?;
        }
        if source.follow_up == super::voice_source::FollowUp::Unanswered {
            break;
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
        source.answer_due = generation_options.response_deadline;
    }
    if source.session.ending() {
        tokio::time::timeout(options.played_timeout, flush_end(port))
            .await
            .map_err(|_| Error::Deadline)??;
    }
    Ok(())
}

async fn flush_end(port: &mut impl VoicePort) -> Result<(), Error> {
    if crate::bridge_endpoint::voice::close_requested() {
        port.end_remote().await
    } else {
        port.flush_session_answer().await
    }
}

fn settle_cut(
    source: &mut Source,
    followup: bool,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<bool, Error> {
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
    // A granted end closes at the settled reply boundary.
    if source.session.ending() {
        return Ok(false);
    }
    let mut followup = options.listen_timeout.is_some();
    if followup && !source.delegation.waiting() && crate::bridge_endpoint::remote_start::take_followup() {
        let deadline = (Instant::now() + Duration::from_secs(2)).min(source.run_ceiling.ok_or(Error::Deadline)?);
        let next = super::remote_admission::followup(client, events, cancelled.as_mut(), deadline, |event| {
            super::device(event, true, source, observe)
        })
        .await?;
        if let Some(run) = next {
            *microphone = run;
            super::earcon::reset_capture();
            source.controller.microphone_started();
        } else {
            followup = false;
        }
        if crate::bridge_endpoint::voice::close_requested() {
            source.session.request_end();
            return Ok(false);
        }
    }
    if !followup && !source.delegation.waiting() {
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

/// A delegated turn ended. The hand-off never says why the voice delegated,
/// so the runner's own `end_session` call is the only end authority. Once it
/// is applied and the runner finishes with nothing to hand back, the reply
/// owes no delegated answer: the farewell already spoken is all of it.
pub(super) async fn complete_turn(answered: bool, port: &mut impl VoicePort, source: &mut Source) -> Result<(), Error> {
    source.delegation.completed();
    let silent = !answered && !source.delegation.in_flight();
    if silent && source.session.ending() && source.delegation.released() {
        close_reply(port, source).await?;
    }
    Ok(())
}

/// An applied end: stop what it supersedes, and finish a farewell that was
/// only held open for the delegated answer.
pub(super) async fn end_if_ended(port: &mut impl VoicePort, source: &mut Source) -> Result<(), Error> {
    if source.end_if_ended(port)? {
        close_reply(port, source).await?;
    }
    Ok(())
}

/// Finish the reply after the audio that has already arrived.
async fn close_reply(port: &mut impl VoicePort, source: &mut Source) -> Result<(), Error> {
    if !source.reply.finished() {
        while let Some(frame) = port.ready_audio().await? {
            source.audio(&frame)?;
            tokio::task::yield_now().await;
        }
        source.reply.finish()?;
    }
    Ok(())
}

async fn clock_step(drain: &mut Option<crate::bridge_mcp::bridge_clock::receipt::VoiceDrain>) {
    match drain {
        Some(drain) => drain.acknowledge().await,
        None => std::future::pending().await,
    }
}

/// A silent gap in a running turn is Thinking, even with more playout queued.
pub(super) fn playout_notice(reply: &crate::bridge_codex::ReplyAudio, delegating: bool) -> Option<Notice> {
    Some(match (reply.mouth_change()?, delegating) {
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
    super::voice_wait::run_marks::report(source, observe)?;
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
    *stats.quality = source.settle(port.rtp_lost());
    source.follow_up = super::voice_source::FollowUp::Replied;
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
    if !source.delegation.answered() {
        return Ok(());
    }
    close_reply(port, source).await
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
    let mut turn_started = false;
    loop {
        super::voice_wait::run_marks::report(source, observe)?;
        // A granted extension moves the deadline; a requested end closes now.
        let idle = source.controller.idle_deadline().ok_or(Error::Input)?;
        let delegated = source.delegation.waiting();
        let (deadline, expiry) = super::voice_wait::listening(source, idle);
        if source.session.ending() || (!followup && !delegated) {
            return Ok(false);
        }
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            () = crate::bridge_endpoint::voice::ending() => {
                source.session.request_end();
                port.retire_audio();
                return Ok(false);
            }
            // Playback has settled before entering listen. A running delegated
            // turn still owns this reply; finish it before admitting cleanup.
            () = crate::bridge_settings::settings_restart::requested(), if !source.delegation.in_flight() => return Ok(false),
            () = until(deadline) => {
                if let Some(expiry) = super::voice_wait::listen_lapsed(source, expiry) { return expiry; }
            },
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
                    end_if_ended(port, source).await?;
                }
                MediaEvent::Control(Event::Closed | Event::Tool { .. }) => return Err(Error::Unavailable),
                MediaEvent::Control(Event::TurnCompleted { .. }) => source.dispatch_completed(),
                MediaEvent::Control(Event::SpeechStopped) => source.user_quiet(),
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
        MediaEvent::Control(Event::SpeechStarted | Event::ReplyHandoff(_) | Event::TurnStarted { .. })
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
        MediaEvent::Control(Event::ReplyHandoff(_) | Event::TurnStarted { .. })
    ) || matches!(event, MediaEvent::Control(Event::Transcript { role, .. }) if role == "assistant")
}

#[cfg(test)]
#[path = "tests/voice_listen_tests.rs"]
mod tests;
