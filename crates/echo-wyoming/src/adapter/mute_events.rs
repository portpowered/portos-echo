//! Cancellation-safe mute ordering and transport-owned connection barriers.
use super::{Error, EventStream, LinkEvent, LinkEvents, Run, Speaker, WyomingEvent, error};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    Ended,
    Open,
}

pub struct Events {
    raw: EventStream,
    current: Option<Arc<crate::wyoming_queue::MicQueue>>,
    pending: Option<crate::ReceivedEvent>,
    generation: Option<Arc<()>>,
    muted: bool,
    capture: Capture,
    handshake: bool,
    barrier: bool,
    speaker: Speaker,
}
impl Events {
    pub(super) fn new(raw: EventStream, handshake: bool, speaker: Speaker) -> Self {
        Self {
            raw,
            current: None,
            pending: None,
            generation: None,
            muted: false,
            capture: Capture::Ended,
            handshake,
            barrier: handshake,
            speaker,
        }
    }
    fn generation(&mut self) -> bool {
        let Some(event) = &self.pending else {
            return false;
        };
        if self
            .generation
            .as_ref()
            .is_some_and(|epoch| Arc::ptr_eq(epoch, &event.generation))
        {
            return false;
        }
        self.generation = Some(event.generation.clone());
        self.current = None;
        self.capture = Capture::Ended;
        self.barrier = self.handshake;
        self.speaker.reset();
        let reset = self.muted
            && !matches!(&event.event,
            WyomingEvent::Other(name, data) if name == "hassmic-mute" && data["muted"] == true);
        self.muted = false;
        reset
    }
    async fn next(&mut self) -> Result<Option<LinkEvent<Run>>, Error> {
        if self.pending.is_none() {
            self.pending = self.raw.recv().await.map_err(error)?;
        }
        if self.generation() {
            return Ok(Some(LinkEvent::Mute(false)));
        }
        let Some(event) = &self.pending else {
            return Ok(None);
        };
        let mute = match &event.event {
            WyomingEvent::Other(name, data) if name == "hassmic-mute" => {
                Some(data["muted"].as_bool().ok_or(Error::Protocol)?)
            }
            _ => None,
        };
        if mute == Some(true)
            && let Some(queue) = &self.current
        {
            queue.drained().await;
        }
        let event = self.pending.take().ok_or(Error::Closed)?;
        if let Some(muted) = mute {
            self.muted = muted;
            if muted {
                self.capture = Capture::Ended;
            }
            return Ok(Some(LinkEvent::Mute(muted)));
        }
        if matches!(event.event, WyomingEvent::RunPipeline(_)) {
            return Ok(Some(self.pipeline(event.run)));
        }
        if matches!(event.event, WyomingEvent::AudioChunk(_))
            && (self.capture == Capture::Ended || !event.capture_active)
        {
            return Ok(Some(LinkEvent::MicrophoneDropped(event.audio_count)));
        }
        if matches!(event.event, WyomingEvent::AudioStop(_)) {
            self.capture = Capture::Ended;
        }
        if matches!(event.event, WyomingEvent::Info(_)) {
            self.barrier = false;
        }
        Ok(Some(if matches!(event.event, WyomingEvent::Played(_)) {
            LinkEvent::Played
        } else {
            LinkEvent::Other
        }))
    }

    fn pipeline(&mut self, run: Option<Run>) -> LinkEvent<Run> {
        let Some(run) = run else {
            return LinkEvent::StaleRemoteCapture;
        };
        // A registered peer may wake before the next idle describe probe.
        // Once admitted, its completion must not be filtered as pre-Info noise.
        self.barrier = false;
        self.current = Some(run.queue.clone());
        self.capture = if self.muted { Capture::Ended } else { Capture::Open };
        if let Some(request_id) = run.pipeline().extra.get("requestId").and_then(|id| id.as_str()) {
            LinkEvent::RemoteCapture {
                request_id: request_id.to_owned(),
                run,
            }
        } else {
            LinkEvent::Wake(run)
        }
    }
}
impl LinkEvents for Events {
    type Run = Run;
    async fn recv(&mut self) -> Result<Option<LinkEvent<Run>>, Error> {
        loop {
            // Unknown pre-Info frames never masquerade as the startup barrier.
            if self.pending.is_none() {
                self.pending = self.raw.recv().await.map_err(error)?;
            }
            if self.generation() {
                return Ok(Some(LinkEvent::Mute(false)));
            }
            // Speaker reports update the link's state, before or after Info.
            if self
                .pending
                .as_ref()
                .is_some_and(|event| self.speaker.observe(&event.event))
            {
                self.pending = None;
                continue;
            }
            if self.barrier
                && self.pending.as_ref().is_some_and(|event| {
                    !matches!(
                        &event.event,
                        WyomingEvent::Info(_) | WyomingEvent::RunPipeline(_) | WyomingEvent::AudioChunk(_)
                    ) && !matches!(&event.event, WyomingEvent::Other(name, _) if name == "hassmic-mute")
                })
            {
                self.pending = None;
                continue;
            }
            return self.next().await;
        }
    }
}
