pub(super) mod bridge_live;
use super::bridge_pacing::LeadPacer;
use super::bridge_refusal::{Arbiter, Work};
use super::{bridge_screen::Screen, bridge_timing};
use crate::{
    Error,
    bridge_audio::{self, Microphone, Reply},
    bridge_trace::Trace,
};
use bridge_live::{LivePort, send};
use echo_audio::FrameQueue;
use echo_contracts::live::LiveEvent;
use echo_link::{EchoLink, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PlayOutcome, ReplySink};
use serde_json::json;
use std::{future::Future, num::NonZeroUsize, pin::Pin, time::Duration};
use tokio::time::Instant;
type CutReceipt<'a> = Pin<Box<dyn Future<Output = Result<echo_link::CutStats, echo_link::Error>> + Send + 'a>>;
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
    user_speaking: bool,
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
            user_speaking: false,
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
        screen: &Screen,
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
        // Validate the entire frame before lab-only clipping can hide malformed input.
        if lab && (frame.payload.len() > 8192 || !frame.payload.len().is_multiple_of(2)) {
            return Err(Error::Audio);
        }
        let bytes = if lab {
            frame
                .payload
                .len()
                .min(usize::try_from(10240 - mic.input_samples).map_err(|_| Error::Audio)? * 2)
        } else {
            frame.payload.len()
        };
        let packets = mic.process(&format, &frame.payload[..bytes])?;
        let complete = lab && mic.input_samples == 10240;
        send(call, packets, screen, &mut self.sent).await?;
        if complete {
            self.finish_mic(call, screen).await?;
        }
        Ok(())
    }
    async fn finish_mic(&mut self, call: &mut impl LivePort, screen: &Screen) -> Result<(), Error> {
        if !self.finished {
            if let Some(mic) = &mut self.mic {
                send(call, mic.finish()?, screen, &mut self.sent).await?;
            }
            self.finished = true;
        }
        Ok(())
    }
    fn resume_reply(&mut self) -> Result<(), Error> {
        self.reply = Reply::new()?;
        self.audio_done = false;
        self.cancelled = false;
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
        screen: &Screen,
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
            LiveEvent::Other(value) if value["type"] == "input_audio_buffer.speech_started" => {
                self.user_speaking = true;
                Ok(())
            }
            LiveEvent::Other(value) if value["type"] == "input_audio_buffer.speech_stopped" => {
                self.user_speaking = false;
                Ok(())
            }
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
mod bridge_output;
use bridge_output::Output;
mod bridge_response;
pub(super) use bridge_response::run;
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
#[derive(PartialEq, Eq)]
enum Cancellation {
    Response,
    Shutdown,
}
struct PlaybackContext<'a, L: EchoLink> {
    client: &'a L,
    trace: &'a mut Trace,
    id: &'a str,
    lead_ms: u16,
    screen: &'a Screen,
    speaking: bool,
    microphone: bool,
    microphone_at_reply: Option<u64>,
    played: bool,
    cancellation: Cancellation,
    cut: Option<(CutReceipt<'a>, u64, u64, u64)>,
    wait: bridge_timing::Wait,
    delegation: &'a mut super::bridge_delegation::Host,
    kill: &'a super::bridge_kill::Kill<'a>,
}
impl<L: EchoLink> PlaybackContext<'_, L> {
    async fn retire<T>(
        &mut self,
        operation: impl Future<Output = T>,
        ports: (&mut <L::Events as LinkEvents>::Run, &mut L::Events),
        source: &mut Source,
        call: &mut impl LivePort,
    ) -> Result<T, Error> {
        let (run, events) = ports;
        let mut ended = false;
        let mut arbiter = Arbiter::default();
        tokio::pin!(operation);
        loop {
            match arbiter
                .next(
                    async {
                        if ended {
                            std::future::pending().await
                        } else {
                            run.recv().await
                        }
                    },
                    &mut operation,
                    std::future::pending::<std::convert::Infallible>(),
                    events.recv(),
                )
                .await
            {
                Work::Control(result) => return Ok(result),
                Work::Microphone(frame) => {
                    if self.cancellation == Cancellation::Response {
                        ended = self.capture(source, frame?, call, false).await?;
                    } else {
                        ended = frame?.is_none();
                    }
                }
                Work::Device(event) => self.device(source, &event?.ok_or(Error::Wyoming)?)?,
                Work::Reply(never) => match never {},
            }
        }
    }
    async fn settle_failure(&mut self, error: Error) -> Error {
        // Settle only an issued cut, under its original transport deadline.
        let _ = self.finish_cut().await;
        error
    }
    fn continuing(&self, source: &Source) -> bool {
        source.cancelled
            && self.cancellation == Cancellation::Response
            && self.microphone_at_reply.is_some_and(|before| source.sent > before)
    }
    fn pressure(&mut self, source: &Source, reported: &mut bool) -> Result<(), Error> {
        if !*reported && source.queue.drop_count() > 0 {
            self.trace
                .record("reply.overflow", Some(self.id), json!({"capacity":128}))?;
            *reported = true;
        }
        Ok(())
    }
    async fn cancelled(
        &mut self,
        source: &mut Source,
        call: &mut impl LivePort,
        output: &mut Output<'_, L>,
        run: &mut <L::Events as LinkEvents>::Run,
        events: &mut L::Events,
    ) -> Result<bool, Error> {
        if self.kill.sync(self.trace).await? {
            self.cancellation = Cancellation::Shutdown;
            source.cancelled = true;
            call.close();
        }
        if source.cancelled {
            self.screen.presentation.clear_emotion()?;
            // These packets were admitted for the response being cancelled.
            // Drain them before cut can await Played and admit a replacement.
            while call.pop_audio().is_some() {}
            self.cut(source, output.discard());
            self.await_transport(events, run, source, call, true).await?;
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
            // A cancelled reply without new admitted input has no replacement
            // response. Keep its partial microphone packet unflushed.
            if !source.cancelled || self.continuing(source) {
                source.finish_mic(call, self.screen).await?;
            }
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
            if Instant::now() < self.wait.cutoff
                && self.delegation.dispatch(event, self.id, self.trace, self.wait.cutoff)?
            {
                self.wait.delegation_started();
            }
            Ok(())
        } else {
            source.event(event, lab)?;
            // Lab echo has one fixed input, never an interrupting question.
            // Fairly interleaved packets after reply start are still that input.
            if lab && source.cancelled {
                self.microphone_at_reply = None;
            }
            Ok(())
        }
    }
    fn cut(&mut self, source: &mut Source, pending: u64) {
        let bridge_evicted = source.queue.drop_count();
        self.cut = Some((
            Box::pin(self.client.cut_with_stats()),
            source.discard_reply() + pending,
            bridge_evicted,
            source.framed,
        ));
    }
    async fn finish_cut(&mut self) -> Result<(), Error> {
        let Some((receipt, ..)) = &mut self.cut else {
            return Ok(());
        };
        let result = receipt.await;
        let (_, bridge_dropped, bridge_evicted, framed) = self.cut.take().ok_or(Error::Wyoming)?;
        let stats = result?;
        self.trace.record(
            "reply.cut",
            Some(self.id),
            json!({
                "bridge_dropped":bridge_dropped, "transport_dropped":stats.dropped_frames,
                "dropped_frames":bridge_dropped + stats.dropped_frames,
                "bridge_evicted":bridge_evicted, "reply_frames":framed
            }),
        )
    }
}
#[cfg(test)]
mod bridge_link;
#[cfg(test)]
mod tests;
