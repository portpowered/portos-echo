pub(super) mod bridge_live;
use super::bridge_pacing::LeadPacer;
use super::bridge_timing;
use crate::{
    Error,
    bridge_audio::{self, Microphone, Reply},
    bridge_trace::Trace,
};
use bridge_live::{LivePort, send};
use echo_audio::FrameQueue;
use echo_contracts::live::LiveEvent;
use echo_link::{EchoLink, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PlayOutcome, ReplySink};
use echo_live::call::CallError;
use serde_json::json;
use std::{num::NonZeroUsize, time::Duration};
use tokio::time::Instant;
struct Source {
    mic: Option<Microphone>,
    chunks: u64,
    sent: u64,
    call_packets_base: u64,
    received: u64,
    framed: u64,
    acknowledged: u64,
    finished: bool,
    audio_done: bool,
    cancelled: bool,
    reply: Reply,
    queue: FrameQueue<Vec<u8>>,
    partial: Vec<u8>,
}
impl Source {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            mic: None,
            chunks: 0,
            sent: 0,
            call_packets_base: 0,
            received: 0,
            framed: 0,
            acknowledged: 0,
            finished: false,
            audio_done: false,
            cancelled: false,
            reply: Reply::new()?,
            queue: FrameQueue::new(NonZeroUsize::new(128).ok_or(Error::Audio)?),
            partial: Vec::with_capacity(960),
        })
    }
    async fn microphone(
        &mut self,
        frame: MicFrame,
        call: &mut impl LivePort,
        lab: bool,
        screen: &super::bridge_screen::Screen,
    ) -> Result<(), Error> {
        self.chunks += 1;
        if screen.muted()? {
            screen.received()?;
            return Ok(());
        }
        if self.finished {
            return Ok(());
        }
        let format = frame.format;
        if self.mic.is_none() {
            let mut mic = Microphone::new(format)?;
            mic.packets = self.call_packets_base + self.sent;
            self.mic = Some(mic);
        }
        let mic = self.mic.as_mut().ok_or(Error::Audio)?;
        let packets = mic.process(&format, &frame.payload)?;
        send(call, packets, screen, &mut self.sent).await?;
        if lab && self.chunks == 10 {
            self.finish_mic(call, screen).await?;
        }
        Ok(())
    }
    async fn finish_mic(
        &mut self,
        call: &mut impl LivePort,
        screen: &super::bridge_screen::Screen,
    ) -> Result<(), Error> {
        if !self.finished {
            if let Some(mic) = &mut self.mic {
                send(call, mic.finish()?, screen, &mut self.sent).await?;
            }
            self.finished = true;
        }
        Ok(())
    }
    fn record(&self, trace: &mut Trace, id: &str, stats: echo_link::RunStats, completed: bool) -> Result<(), Error> {
        trace.record("wyoming.mic_frames", Some(id), json!({"count":stats.received}))?;
        let (samples, tail) = self
            .mic
            .as_ref()
            .map_or((0, 0), |m| (m.input_samples, m.tail_samples()));
        trace.record(
            "mic.completed",
            Some(id),
            json!({"count":self.sent,"received_chunks":stats.received,
        "dropped_chunks":stats.dropped,"input_samples":samples,"tail_samples":tail}),
        )?;
        if completed {
            trace.record("mic.frames", Some(id), json!({"phase":"completed","count":self.sent}))?;
        }
        trace.record("wyoming.run_ended", Some(id), json!({}))?;
        Ok(())
    }
    async fn completion(
        &mut self,
        completed: bool,
        drained: bool,
        echo_done: bool,
        call: &mut impl LivePort,
        screen: &super::bridge_screen::Screen,
    ) -> Result<bool, Error> {
        if completed || !drained || !(self.audio_done || echo_done) {
            return Ok(completed);
        }
        self.finish_mic(call, screen).await?;
        let bytes = self.reply.finish()?;
        self.enqueue(&bytes);
        Ok(true)
    }
    fn echo_done(&self, lab: (bool, u64)) -> bool {
        lab.0 && self.finished && self.sent > 0 && self.received == self.sent && self.acknowledged == self.sent + lab.1
    }
    fn enqueue(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.partial.push(byte);
            if self.partial.len() == 960 {
                self.framed += 1;
                self.queue
                    .push(std::mem::replace(&mut self.partial, Vec::with_capacity(960)));
            }
        }
    }
    fn discard_reply(&mut self) -> u64 {
        let retained = self.queue.len() as u64;
        self.queue.clear();
        self.partial.clear();
        retained
    }
    async fn receive(&mut self, call: &mut impl LivePort) -> Result<bool, Error> {
        // Leave time for the playback and Wyoming event futures in try_join!.
        // A provider burst must not monopolize the owner with synchronous
        // decoding/resampling while the bounded control queue fills up.
        for _ in 0..8 {
            let Some(frame) = call.pop_audio() else {
                return Ok(true);
            };
            self.received += 1;
            let bytes = self.reply.process(&frame)?;
            self.enqueue(&bytes);
        }
        tokio::task::yield_now().await;
        Ok(false)
    }
    fn event(&mut self, event: LiveEvent, lab: bool) -> Result<(), Error> {
        match event {
            LiveEvent::Error { .. } => Err(Error::Unavailable),
            LiveEvent::Other(value)
                if value["type"] == "response.done" && value["response"]["status"] == "cancelled" =>
            {
                self.cancelled = true;
                Ok(())
            }
            // Provisional passthrough vocabulary selected by the packet. M2 owns
            // captured provider compatibility; unknown events never close audio.
            LiveEvent::Other(value) if value["type"] == "response.audio.done" => {
                self.audio_done = true;
                Ok(())
            }
            LiveEvent::Other(value) if lab && value["type"] == "fake.audio.frames" => {
                self.acknowledged = value["count"].as_u64().ok_or(Error::Decode)?;
                Ok(())
            }
            _ => Ok(()), // Unknown vocabulary is never a completion signal.
        }
    }
}
pub(super) async fn run<L: EchoLink>(
    client: &L,
    events: &mut L::Events,
    mut run: <L::Events as LinkEvents>::Run,
    call: &mut impl LivePort,
    trace: &mut Trace,
    id: &str,
    options: (
        bool,
        u16,
        u64,
        &super::bridge_screen::Screen,
        &mut super::bridge_delegation::Host,
        &super::bridge_kill::Kill<'_>,
    ),
) -> Result<(u64, bool), Error> {
    let mut source = Source::new()?;
    source.call_packets_base = options.2;
    let (tx, rx) = client.reply_channel(NonZeroUsize::new(1).ok_or(Error::Audio)?);
    let budget = bridge_timing::playback(options.5.settings);
    let play = client.play_with_timeout(bridge_audio::format(24_000), rx, budget);
    let mut context = PlaybackContext {
        client,
        trace,
        id,
        lead_ms: options.1,
        screen: options.3,
        speaking: false,
        microphone: false,
        played: false,
        deadline: Instant::now() + bridge_timing::RESPONSE,
        delegation: options.4,
        kill: options.5,
    };
    let mut failure = None;
    let producer = async {
        let result = produce(
            &mut source,
            &mut run,
            events,
            call,
            tx,
            (options.0, options.2),
            &mut context,
        )
        .await;
        failure = stop_failed_producer(result, client, options.0).await?;
        context.await_played(events, &mut run, &mut source, call).await
    };
    let (outcome, ()) = tokio::try_join!(async { play.await.map_err(Error::from) }, producer)?;
    failure.map_or(Ok(()), Err)?;
    let abandoned = context.delegation.abandon().await;
    context.screen.presentation.clear_emotion()?;
    trace.record(
        "reply.played",
        Some(id),
        json!({"outcome":if outcome == PlayOutcome::Cut { "cut" } else { "played" }}),
    )?;
    tokio::time::timeout(Duration::from_secs(1), async {
        while run.recv().await?.is_some() {}
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| Error::Deadline)??;
    source.record(trace, id, run.stats(), !source.cancelled && !abandoned)?;
    Ok((source.sent, !source.cancelled && !abandoned))
}
async fn stop_failed_producer<L: EchoLink>(
    result: Result<(), Error>,
    client: &L,
    lab: bool,
) -> Result<Option<Error>, Error> {
    match result {
        Ok(()) => Ok(None),
        Err(error) if lab => Err(error), // Preserve the lab caller's fatal-error protocol.
        Err(error) => {
            // Keep play's receiver alive through cut and Played. Dropping it
            // first invalidates the Wyoming connection and loses the stop.
            client.cut_with_stats().await?;
            Ok(Some(error))
        }
    }
}
struct PlaybackContext<'a, L: EchoLink> {
    client: &'a L,
    trace: &'a mut Trace,
    id: &'a str,
    lead_ms: u16,
    screen: &'a super::bridge_screen::Screen,
    speaking: bool,
    microphone: bool,
    played: bool,
    deadline: Instant,
    delegation: &'a mut super::bridge_delegation::Host,
    kill: &'a super::bridge_kill::Kill<'a>,
}
impl<L: EchoLink> PlaybackContext<'_, L> {
    fn pressure(&mut self, source: &Source, reported: &mut bool) -> Result<(), Error> {
        if !*reported && source.queue.drop_count() > 0 {
            self.trace
                .record("reply.overflow", Some(self.id), json!({"capacity":128}))?;
            *reported = true;
        }
        Ok(())
    }
    async fn await_played(
        &mut self,
        events: &mut L::Events,
        run: &mut <L::Events as LinkEvents>::Run,
        source: &mut Source,
        call: &mut impl LivePort,
    ) -> Result<(), Error> {
        if self.played {
            return Ok(());
        }
        let deadline = Instant::now() + bridge_timing::CONTROL;
        let mut ended = false;
        loop {
            tokio::select! {
                biased;
                event = events.recv() => {
                    let event = event?.ok_or(Error::Wyoming)?;
                    self.screen.observe(&event, self.trace)?;
                    match event {
                        LinkEvent::Played => return Ok(()),
                        LinkEvent::Wake(_) => return Err(Error::Wyoming),
                        _ => (),
                    }
                },
                frame = run.recv(), if !ended => { ended = frame?.is_none(); },
                () = tokio::time::sleep_until(deadline) => return Err(Error::Deadline),
                () = self.kill.wait(), if !source.cancelled => {
                    self.kill.sync(self.trace).await?;
                    source.cancelled = true;
                    self.delegation.abandon().await;
                    call.close();
                }
            }
        }
    }
    async fn cancelled(&mut self, source: &mut Source, call: &mut impl LivePort) -> Result<bool, Error> {
        if self.kill.sync(self.trace).await? {
            source.cancelled = true;
            call.close();
        }
        if source.cancelled {
            self.screen.presentation.clear_emotion()?;
            self.cut(source).await?;
            self.delegation.abandon().await;
        }
        Ok(source.cancelled)
    }
    async fn microphone(
        &mut self,
        source: &mut Source,
        frame: MicFrame,
        call: &mut impl LivePort,
        lab: bool,
    ) -> Result<(), Error> {
        let result = source.microphone(frame, call, lab, self.screen).await;
        self.microphone_activity(source)?;
        result
    }
    fn microphone_activity(&mut self, source: &Source) -> Result<(), Error> {
        if !self.microphone && source.sent > 0 {
            self.trace.record(
                "mic.frames",
                Some(self.id),
                json!({"phase":"first","count":source.sent}),
            )?;
            self.microphone = true;
        }
        Ok(())
    }
    async fn capture(
        &mut self,
        source: &mut Source,
        frame: Option<MicFrame>,
        call: &mut impl LivePort,
        lab: bool,
    ) -> Result<bool, Error> {
        if let Some(frame) = frame {
            self.microphone(source, frame, call, lab).await?;
            Ok(false)
        } else {
            source.finished = true;
            Ok(true)
        }
    }
    fn device(&mut self, source: &mut Source, event: &LinkEvent<<L::Events as LinkEvents>::Run>) -> Result<(), Error> {
        self.screen.observe(event, self.trace)?;
        match event {
            LinkEvent::Mute(true) => source.mic = None,
            LinkEvent::Played => self.played = true,
            LinkEvent::Wake(_) => return Err(Error::Wyoming),
            _ => (),
        }
        Ok(())
    }
    fn event(&mut self, event: LiveEvent, source: &mut Source, lab: bool) -> Result<(), Error> {
        if matches!(event, LiveEvent::DelegationCreated { .. }) {
            if self.delegation.dispatch(event, self.id, self.trace)? {
                self.deadline += bridge_timing::task(self.kill.settings);
            }
            Ok(())
        } else {
            source.event(event, lab)
        }
    }
    async fn forward(&mut self, tx: &L::ReplySink, frame: Vec<u8>) -> Result<(), Error> {
        tx.send(frame).await.map_err(|_| Error::Wyoming)?;
        if !self.speaking {
            self.trace.record("reply.started", Some(self.id), json!({}))?;
            self.screen.publish(echo_screen_contract::UiState::Speaking)?;
            self.speaking = true;
        }
        Ok(())
    }
    async fn forward_available(&mut self, tx: &L::ReplySink, frame: Option<Vec<u8>>) -> Result<(), Error> {
        if let Some(frame) = frame {
            self.forward(tx, frame).await?;
        }
        Ok(())
    }

    async fn cut(&mut self, source: &mut Source) -> Result<(), Error> {
        let bridge_evicted = source.queue.drop_count();
        let bridge_dropped = source.discard_reply();
        // Keep the source sender alive through this request: EOF must not race
        // the cut and turn its acknowledgment into normal completion.
        let stats = self.client.cut_with_stats().await?;
        self.trace.record(
            "reply.cut",
            Some(self.id),
            json!({
                "bridge_dropped":bridge_dropped, "transport_dropped":stats.dropped_frames,
                "dropped_frames":bridge_dropped + stats.dropped_frames,
                "bridge_evicted":bridge_evicted, "reply_frames":source.framed
            }),
        )
    }
}
async fn produce<L: EchoLink>(
    source: &mut Source,
    run: &mut <L::Events as LinkEvents>::Run,
    events: &mut L::Events,
    call: &mut impl LivePort,
    tx: L::ReplySink,
    lab: (bool, u64),
    context: &mut PlaybackContext<'_, L>,
) -> Result<(), Error> {
    let mut pacer = LeadPacer::new(context.lead_ms);
    let mut completed = false;
    let mut ended = false;
    let mut pressure_reported = false;
    loop {
        if context.cancelled(source, call).await? {
            return Ok(());
        }
        context.delegation.service(call, context.trace, context.screen).await?;
        context.screen.presentation.expire_emotion()?;
        let drained = source.receive(call).await?;
        context.pressure(source, &mut pressure_reported)?;
        let echo_done = source.echo_done(lab);
        completed = source
            .completion(completed, drained, echo_done, call, context.screen)
            .await?;
        context.microphone_activity(source)?;
        if completed && source.queue.is_empty() {
            return Ok(());
        }
        bridge_timing::check_deadline(context.deadline)?;
        tokio::select! {
            biased;
            event = events.recv() => {
                let event = event?.ok_or(Error::Wyoming)?;
                context.device(source, &event)?;
            },
            frame = run.recv(), if !ended => { ended = context.capture(source, frame?, call, lab.0).await?; },
            () = context.delegation.ready() => (),
            () = context.screen.presentation.emotion_deadline() => (),
            event = call.receive_event((Instant::now() + Duration::from_millis(20)).min(context.deadline).into_std()) => match event {
                Ok(event) => context.event(event, source, lab.0)?,
                Err(CallError::Deadline) => (),
                Err(error) => return Err(error.into()),
            },
            frame = pacer.next_frame(&mut source.queue), if !source.queue.is_empty() => {
                context.forward_available(&tx, frame).await?;
            }
        }
    }
}
#[cfg(test)]
mod bridge_link;
#[cfg(test)]
mod tests;
