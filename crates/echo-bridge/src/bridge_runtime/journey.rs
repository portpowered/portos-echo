use super::bridge_pacing::LeadPacer;
use crate::{
    Error,
    bridge_audio::{self, Microphone, Reply},
    bridge_trace::Trace,
};
use echo_audio::FrameQueue;
use echo_contracts::live::LiveEvent;
use echo_live::{
    AudioFrame,
    call::{CallError, LiveCall},
};
use echo_wyoming::{EventStream, MicFrame, Run, WyomingClient};
use serde_json::json;
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

struct Source {
    mic: Option<Microphone>,
    chunks: u64,
    sent: u64,
    received: u64,
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
            received: 0,
            acknowledged: 0,
            finished: false,
            audio_done: false,
            cancelled: false,
            reply: Reply::new()?,
            queue: FrameQueue::new(NonZeroUsize::new(128).ok_or(Error::Audio)?),
            partial: Vec::with_capacity(960),
        })
    }

    async fn microphone(&mut self, frame: MicFrame, call: &mut LiveCall, lab: bool) -> Result<(), Error> {
        self.chunks += 1;
        if self.finished {
            return Ok(());
        }
        let format = echo_contracts::wyoming::AudioFormat {
            rate: frame.data.rate,
            width: frame.data.width,
            channels: frame.data.channels,
            extra: frame.data.extra,
        };
        if self.mic.is_none() {
            self.mic = Some(Microphone::new(format.clone())?);
        }
        let mic = self.mic.as_mut().ok_or(Error::Audio)?;
        let packets = mic.process(&format, &frame.payload)?;
        self.sent += send(call, packets).await?;
        if lab && self.chunks == 10 {
            self.finish_mic(call).await?;
        }
        Ok(())
    }

    async fn finish_mic(&mut self, call: &mut LiveCall) -> Result<(), Error> {
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
                self.queue
                    .push(std::mem::replace(&mut self.partial, Vec::with_capacity(960)));
            }
        }
    }

    fn receive(&mut self, call: &mut LiveCall) -> Result<(), Error> {
        while let Some(frame) = call.pop_audio() {
            self.received += 1;
            let bytes = self.reply.process(&frame)?;
            self.enqueue(&bytes);
        }
        Ok(())
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

pub(super) async fn run(
    client: &WyomingClient,
    events: &mut EventStream,
    mut run: Run,
    call: &mut LiveCall,
    trace: &mut Trace,
    id: &str,
    options: (bool, u16),
) -> Result<(), Error> {
    let mut source = Source::new()?;
    let (tx, rx) = mpsc::channel(1);
    trace.record("reply.started", Some(id), json!({}))?;
    let play = client.play(bridge_audio::format(24_000), rx);
    let mut context = PlaybackContext {
        client,
        trace,
        id,
        lead_ms: options.1,
    };
    let producer = produce(&mut source, &mut run, call, tx, options.0, &mut context);
    let (outcome, (), ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            async { play.await.map_err(Error::from) },
            producer,
            super::played(events)
        )
    })
    .await
    .map_err(|_| Error::Deadline)??;
    trace.record(
        "reply.played",
        Some(id),
        json!({"outcome":if outcome == echo_wyoming::PlayOutcome::Cut { "cut" } else { "played" }}),
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
        "mic.frames",
        Some(id),
        json!({"count":source.sent,"received_chunks":stats.received,
        "dropped_chunks":stats.dropped,"input_samples":samples,"tail_samples":tail}),
    )?;
    trace.record("wyoming.run_ended", Some(id), json!({}))
}

struct PlaybackContext<'a> {
    client: &'a WyomingClient,
    trace: &'a mut Trace,
    id: &'a str,
    lead_ms: u16,
}

impl PlaybackContext<'_> {
    async fn cut(&mut self, source: &mut Source) -> Result<(), Error> {
        let bridge_dropped = source.queue.len() as u64;
        source.queue.clear();
        source.partial.clear();
        // Keep the source sender alive through this request: EOF must not race
        // the cut and turn its acknowledgment into normal completion.
        let stats = self.client.cut_with_stats().await?;
        self.trace.record(
            "reply.cut",
            Some(self.id),
            json!({
                "bridge_dropped":bridge_dropped, "transport_dropped":stats.dropped_frames,
                "dropped_frames":bridge_dropped + stats.dropped_frames
            }),
        )
    }
}

async fn produce(
    source: &mut Source,
    run: &mut Run,
    call: &mut LiveCall,
    tx: mpsc::Sender<Vec<u8>>,
    lab: bool,
    context: &mut PlaybackContext<'_>,
) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pacer = LeadPacer::new(context.lead_ms);
    let mut completed = false;
    loop {
        if source.cancelled {
            context.cut(source).await?;
            return Ok(());
        }
        source.receive(call)?;
        let echo_done = lab
            && source.finished
            && source.sent > 0
            && source.received == source.sent
            && source.acknowledged == source.sent;
        if !completed && (source.audio_done || echo_done) {
            source.finish_mic(call).await?;
            let bytes = source.reply.finish()?;
            source.enqueue(&bytes);
            completed = true;
        }
        if completed && source.queue.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        tokio::select! {
            frame = run.recv() => match frame? {
                Some(frame) => source.microphone(frame, call, lab).await?,
                None => return Err(Error::Wyoming),
            },
            event = call.receive_event((Instant::now() + Duration::from_millis(20)).min(deadline)) => match event {
                Ok(event) => source.event(event, lab)?,
                Err(CallError::Deadline) => (),
                Err(error) => return Err(error.into()),
            },
            frame = pacer.next_frame(&mut source.queue), if !source.queue.is_empty() => {
                if let Some(frame) = frame { tx.send(frame).await.map_err(|_| Error::Wyoming)?; }
            }
        }
    }
}

async fn send(call: &mut LiveCall, packets: Vec<AudioFrame>) -> Result<u64, Error> {
    let mut count = 0;
    for packet in packets {
        call.send_audio(packet, Instant::now()).await?;
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
