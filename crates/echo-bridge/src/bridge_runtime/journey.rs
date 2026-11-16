use crate::{
    Error,
    bridge_audio::{self, Microphone, Reply},
    bridge_trace::Trace,
};
use echo_audio::{FrameQueue, Pacer};
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
    lab: bool,
) -> Result<(), Error> {
    let mut source = Source::new()?;
    let (tx, rx) = mpsc::channel(1);
    trace.record("reply.started", Some(id), json!({}))?;
    let play = client.play(bridge_audio::format(24_000), rx);
    let producer = produce(&mut source, &mut run, call, tx, lab);
    let (_, (), ()) = tokio::try_join!(
        async { play.await.map_err(Error::from) },
        producer,
        super::played(events)
    )?;
    trace.record("reply.played", Some(id), json!({"outcome":"played"}))?;
    while run.recv().await?.is_some() {}
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

async fn produce(
    source: &mut Source,
    run: &mut Run,
    call: &mut LiveCall,
    tx: mpsc::Sender<Vec<u8>>,
    lab: bool,
) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pacer = Pacer::new();
    let mut completed = false;
    loop {
        source.receive(call)?;
        if !completed && lab && source.finished && source.received == source.sent && source.acknowledged == source.sent
        {
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
