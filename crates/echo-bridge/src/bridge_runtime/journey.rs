pub(super) mod bridge_live;
use super::bridge_pacing::LeadPacer;
use super::bridge_timing;
use crate::{
    Error,
    bridge_audio::{self, Microphone, Reply},
    bridge_trace::Trace,
};
use bridge_live::LivePort;
use echo_audio::FrameQueue;
use echo_contracts::live::LiveEvent;
use echo_link::{EchoLink, LinkEvents, MicFrame, MicrophoneRun, PlayOutcome, ReplySink};
use echo_live::{AudioFrame, call::CallError};
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

    async fn microphone(&mut self, frame: MicFrame, call: &mut impl LivePort, lab: bool) -> Result<(), Error> {
        self.chunks += 1;
        if self.finished {
            return Ok(());
        }
        let format = frame.format;
        if self.mic.is_none() {
            let mut mic = Microphone::new(format)?;
            mic.packets = self.call_packets_base;
            self.mic = Some(mic);
        }
        let mic = self.mic.as_mut().ok_or(Error::Audio)?;
        let packets = mic.process(&format, &frame.payload)?;
        self.sent += send(call, packets).await?;
        if lab && self.chunks == 10 {
            self.finish_mic(call).await?;
        }
        Ok(())
    }

    async fn finish_mic(&mut self, call: &mut impl LivePort) -> Result<(), Error> {
        if !self.finished {
            if let Some(mic) = &mut self.mic {
                self.sent += send(call, mic.finish()?).await?;
            }
            self.finished = true;
        }
        Ok(())
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
        deadline: Instant::now() + bridge_timing::RESPONSE,
        delegation: options.4,
        kill: options.5,
    };
    let mut failure = None;
    let (acknowledged, acknowledgement) = tokio::sync::oneshot::channel();
    let producer = async {
        let result = produce(&mut source, &mut run, call, tx, (options.0, options.2), &mut context).await;
        failure = stop_failed_producer(result, client, options.0).await?;
        context.await_played(acknowledgement, &mut source, call).await
    };
    let (outcome, (), ()) = tokio::try_join!(async { play.await.map_err(Error::from) }, producer, async {
        let result = super::played(events).await;
        let _ = acknowledged.send(());
        result
    })?;
    failure.map_or(Ok(()), Err)?;
    let abandoned = context.delegation.abandon();
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
    let stats = run.stats();
    trace.record("wyoming.mic_frames", Some(id), json!({"count":stats.received}))?;
    let (samples, tail) = source
        .mic
        .as_ref()
        .map_or((0, 0), |m| (m.input_samples, m.tail_samples()));
    trace.record(
        "mic.completed",
        Some(id),
        json!({"count":source.sent,"received_chunks":stats.received,
        "dropped_chunks":stats.dropped,"input_samples":samples,"tail_samples":tail}),
    )?;
    trace.record("wyoming.run_ended", Some(id), json!({}))?;
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
    deadline: Instant,
    delegation: &'a mut super::bridge_delegation::Host,
    kill: &'a super::bridge_kill::Kill<'a>,
}

impl<L: EchoLink> PlaybackContext<'_, L> {
    async fn await_played(
        &mut self,
        mut acknowledgement: tokio::sync::oneshot::Receiver<()>,
        source: &mut Source,
        call: &mut impl LivePort,
    ) -> Result<(), Error> {
        let deadline = Instant::now() + bridge_timing::CONTROL;
        loop {
            tokio::select! {
                biased;
                _ = &mut acknowledgement => return Ok(()),
                () = tokio::time::sleep_until(deadline) => return Err(Error::Deadline),
                () = self.kill.wait(), if !source.cancelled => {
                    self.kill.sync(self.trace).await?;
                    source.cancelled = true;
                    self.delegation.abandon();
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
            self.delegation.abandon();
            self.cut(source).await?;
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
        source.microphone(frame, call, lab).await?;
        if !self.microphone && source.sent > 0 {
            self.trace
                .record("mic.frames", Some(self.id), json!({"count":source.sent}))?;
            self.microphone = true;
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
    call: &mut impl LivePort,
    tx: L::ReplySink,
    lab: (bool, u64),
    context: &mut PlaybackContext<'_, L>,
) -> Result<(), Error> {
    let mut pacer = LeadPacer::new(context.lead_ms);
    let mut completed = false;
    let mut pressure_reported = false;
    loop {
        if context.cancelled(source, call).await? {
            return Ok(());
        }
        context.delegation.service(call, context.trace).await?;
        let drained = source.receive(call).await?;
        if !pressure_reported && source.queue.drop_count() > 0 {
            context
                .trace
                .record("reply.overflow", Some(context.id), json!({"capacity":128}))?;
            pressure_reported = true;
        }
        let echo_done = lab.0
            && source.finished
            && source.sent > 0
            && source.received == source.sent
            && source.acknowledged == source.sent + lab.1;
        if !completed && drained && (source.audio_done || echo_done) {
            source.finish_mic(call).await?;
            let bytes = source.reply.finish()?;
            source.enqueue(&bytes);
            completed = true;
        }
        if completed && source.queue.is_empty() {
            return Ok(());
        }
        if Instant::now() >= context.deadline {
            return Err(Error::Deadline);
        }
        tokio::select! {
            frame = run.recv() => match frame? {
                Some(frame) => context.microphone(source, frame, call, lab.0).await?,
                None => return Err(Error::Wyoming),
            },
            event = call.receive_event((Instant::now() + Duration::from_millis(20)).min(context.deadline).into_std()) => match event {
                Ok(event) => context.event(event, source, lab.0)?,
                Err(CallError::Deadline) => (),
                Err(error) => return Err(error.into()),
            },
            frame = pacer.next_frame(&mut source.queue), if !source.queue.is_empty() => {
                if let Some(frame) = frame { context.forward(&tx, frame).await?; }
            }
        }
    }
}

async fn send(call: &mut impl LivePort, packets: Vec<AudioFrame>) -> Result<u64, Error> {
    let mut count = 0;
    for packet in packets {
        call.send_audio(packet, Instant::now().into_std()).await?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod bridge_spine_tests {
    use super::*;

    #[test]
    fn bridge_spine_only_explicit_completion_closes_audio() {
        let mut source = Source::new().unwrap();
        for value in [
            json!({"type":"future.event"}),
            json!({"type":"response.done"}),
            json!({}),
        ] {
            source.event(LiveEvent::Other(value), false).unwrap();
            assert!(!source.audio_done);
        }
        source
            .event(LiveEvent::Other(json!({"type":"response.audio.done"})), false)
            .unwrap();
        assert!(source.audio_done);
        assert_eq!(source.queue.len(), 0);
        assert_eq!(source.reply.finish().unwrap(), Vec::<u8>::new());
        assert_eq!(
            source.event(
                LiveEvent::Error {
                    extra: std::collections::BTreeMap::default()
                },
                false
            ),
            Err(Error::Unavailable)
        );
    }

    #[test]
    fn bridge_spine_echo_acknowledgment_is_lab_only_and_validated() {
        let mut source = Source::new().unwrap();
        let event = LiveEvent::Other(json!({"type":"fake.audio.frames","count":3}));
        source.event(event.clone(), false).unwrap();
        assert_eq!(source.acknowledged, 0);
        source.event(event, true).unwrap();
        assert_eq!(source.acknowledged, 3);
        for count in [json!(-1), json!("3"), json!(null)] {
            assert_eq!(
                source.event(
                    LiveEvent::Other(json!({"type":"fake.audio.frames","count":count})),
                    true
                ),
                Err(Error::Decode)
            );
        }
        assert!(!source.audio_done);
    }

    #[test]
    fn bridge_spine_reply_queue_keeps_newest_complete_frames_and_tail() {
        let mut source = Source::new().unwrap();
        for value in 0..130_u8 {
            source.enqueue(&vec![value; 960]);
        }
        source.enqueue(&[255; 13]);
        assert_eq!(source.partial, vec![255; 13]);
        assert_eq!(source.queue.drop_count(), 2);
        for value in 2..130_u8 {
            assert_eq!(source.queue.pop(), Some(vec![value; 960]));
        }
        assert_eq!(source.queue.pop(), None);
    }
}

#[cfg(test)]
mod bridge_pacing_cut_tests {
    use super::*;

    #[test]
    fn bridge_pacing_cut_excludes_prior_evictions_and_partial_tail() {
        let mut source = Source::new().unwrap();
        for value in 0..130_u8 {
            source.enqueue(&vec![value; 960]);
        }
        source.enqueue(&[255; 13]);
        assert_eq!(source.framed, 130);
        assert_eq!(source.queue.drop_count(), 2);
        assert_eq!(source.discard_reply(), 128);
        assert_eq!(source.queue.drop_count(), 0); // FrameQueue::clear resets its interval statistics.
        assert_eq!(source.queue.pop(), None);
        assert_eq!(source.partial, Vec::<u8>::new());
        assert_eq!(source.discard_reply(), 0);
    }
}

#[cfg(test)]
mod bridge_link;
