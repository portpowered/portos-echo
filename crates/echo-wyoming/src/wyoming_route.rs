use crate::adapter::speaker_events;
use crate::wyoming_client::device::Connection;
use crate::wyoming_events;
use crate::{Error, MicFrame, ReceivedEvent, Run, wyoming_queue::MicQueue};
use echo_contracts::wyoming::{Event, WyomingEvent};
use echo_core_api::DeviceEvent;
use echo_link::SpeakerState;
use std::sync::{Arc, Weak};
use tokio::time::Instant;

pub(crate) struct Router {
    pub(crate) generation: Arc<()>,
    pub(crate) current: Option<Arc<MicQueue>>,
    pub(crate) events: wyoming_events::Sender,
    pub(crate) capacity: usize,
    pub(crate) device: Connection,
    remote: Option<(Weak<str>, Instant)>,
    stale_capture: bool,
}

impl Router {
    #[cfg(test)]
    pub(crate) fn new(events: wyoming_events::Sender, capacity: usize) -> Self {
        let control = Arc::new(crate::wyoming_client::device::Standalone::default());
        Self::with_device(events, capacity, Connection::new(control))
    }

    pub(crate) fn with_device(events: wyoming_events::Sender, capacity: usize, device: Connection) -> Self {
        Self {
            generation: Arc::new(()),
            current: None,
            events,
            capacity,
            device,
            remote: None,
            stale_capture: false,
        }
    }

    pub(crate) fn reserve_remote(&mut self, request_id: Weak<str>, deadline: Instant) {
        self.remote = Some((request_id, deadline));
    }

    pub(crate) fn clear_remote(&mut self) {
        self.remote = None;
    }

    fn accepts_pipeline(&mut self, request_id: Option<&serde_json::Value>) -> bool {
        let Some(request_id) = request_id else {
            return true;
        };
        let matches = !self.device.view().privacy.mic_muted
            && self.remote.as_ref().is_some_and(|(expected, deadline)| {
                Instant::now() < *deadline
                    && expected
                        .upgrade()
                        .is_some_and(|id| request_id.as_str() == Some(id.as_ref()))
            });
        if matches {
            self.clear_remote();
        }
        matches
    }

    pub(crate) fn route(&mut self, mut frame: Event) -> Result<WyomingEvent, Error> {
        let event = frame.typed().map_err(|_| Error::Protocol)?;
        let audio_count = u64::from(matches!(event, WyomingEvent::AudioChunk(_)));
        let speaker = self.observe_speaker(&event);
        let run = match &event {
            WyomingEvent::RunPipeline(pipeline) => self.open(pipeline),
            WyomingEvent::AudioChunk(data) if !self.stale_capture => {
                if let Some(queue) = &self.current {
                    queue.push(MicFrame {
                        data: data.clone(),
                        payload: std::mem::take(&mut frame.payload),
                    });
                }
                None
            }
            WyomingEvent::Other(name, data) if name == "hassmic-mute" => {
                let muted = data["muted"].as_bool().ok_or(Error::Protocol)?;
                self.device.observe(DeviceEvent::MicMuteObserved { muted });
                if muted {
                    self.clear_remote();
                    if let Some(queue) = &self.current {
                        queue.mute();
                    }
                }
                None
            }
            WyomingEvent::AudioStop(_) if !self.stale_capture => {
                self.end(Ok(()));
                None
            }
            _ => None,
        };
        self.events
            .try_send(ReceivedEvent {
                generation: self.generation.clone(),
                audio_count,
                capture_active: self.device.view().capture_open && !self.stale_capture,
                speaker,
                event: event.clone(),
                payload: if audio_count == 0 { frame.payload } else { vec![] },
                run,
            })
            .map_err(|_| Error::Capacity)?;
        Ok(event)
    }

    fn open(&mut self, pipeline: &echo_contracts::wyoming::RunPipelineData) -> Option<Run> {
        self.stale_capture = !self.accepts_pipeline(pipeline.extra.get("requestId"));
        if self.stale_capture {
            return None;
        }
        self.end(Ok(()));
        let view = self.device.observe(DeviceEvent::RunOpened);
        let queue = Arc::new(MicQueue::new(self.capacity));
        if !view.capture_open {
            queue.mute();
        }
        self.current = Some(queue.clone());
        Some(Run {
            pipeline: pipeline.clone(),
            queue,
        })
    }

    fn observe_speaker(&mut self, event: &WyomingEvent) -> Option<SpeakerState> {
        let report = speaker_events::parse(speaker_events::report_data(event)?)?;
        let view = self.device.observe(DeviceEvent::SpeakerObserved {
            volume: report.volume,
            muted: report.muted,
        });
        Some(SpeakerState {
            volume: view.speaker_volume,
            muted: view.speaker_muted,
        })
    }

    pub(crate) fn end(&mut self, result: Result<(), Error>) {
        if let Some(queue) = self.current.take() {
            self.device.observe(DeviceEvent::CaptureChanged { open: false });
            queue.close(result);
        }
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        self.end(Err(Error::Closed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunStats, wyoming_test_support::frame};
    use serde_json::json;
    use std::io::Write;

    fn router() -> (Router, wyoming_events::Receiver) {
        let (events, receiver) = wyoming_events::channel(2);
        (Router::new(events, 2), receiver)
    }

    fn chunk(index: u32) -> Event {
        frame(
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1,"timestamp":index}),
            index.to_le_bytes().to_vec(),
        )
    }

    fn pipeline() -> Event {
        frame(
            "run-pipeline",
            json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"Computer"}),
            vec![],
        )
    }

    #[tokio::test(start_paused = true)]
    async fn audio_flood_preserves_lifecycle_capacity_and_latest_pcm() {
        let (mut router, mut receiver) = router();
        router.route(pipeline()).unwrap();
        for index in 0..10_000 {
            router.route(chunk(index)).unwrap();
        }
        router.route(frame("audio-stop", json!({}), vec![])).unwrap();
        let mut run = receiver.recv().await.unwrap().run.unwrap();
        assert_eq!(
            run.stats(),
            RunStats {
                received: 10_000,
                dropped: 9_998
            }
        );
        for index in [9_998_u32, 9_999] {
            let pcm = run.recv().await.unwrap().unwrap();
            assert_eq!(pcm.payload, index.to_le_bytes());
            assert_eq!(pcm.data.timestamp, Some(u64::from(index)));
        }
        assert_eq!(run.recv().await.unwrap(), None);
        let audio = receiver.recv().await.unwrap();
        assert_eq!(audio.audio_count, 10_000);
        assert_eq!(audio.payload, Vec::<u8>::new());
        assert!(matches!(
            receiver.recv().await.unwrap().event,
            WyomingEvent::AudioStop(_)
        ));
        // The connection is still accepting controls after the flood.
        router.route(frame("ping", json!({}), vec![])).unwrap();
        assert!(matches!(receiver.recv().await.unwrap().event, WyomingEvent::Ping(_)));
        writeln!(
            std::io::stderr(),
            "routed=10000 lifecycle_delivered=1 terminal_capacity=0 received=10000 dropped=9998"
        )
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn audio_flood_without_run_or_after_dropped_run_keeps_next_wake() {
        for dropped in [false, true] {
            let (mut router, mut receiver) = router();
            if dropped {
                router.route(pipeline()).unwrap();
                drop(receiver.recv().await.unwrap().run.unwrap());
            }
            for index in 0..10_000 {
                router.route(chunk(index)).unwrap();
            }
            router.route(pipeline()).unwrap();
            let audio = receiver.recv().await.unwrap();
            assert_eq!(audio.audio_count, 10_000);
            assert_eq!(audio.payload, Vec::<u8>::new());
            let mut next = receiver.recv().await.unwrap().run.unwrap();
            assert_eq!(next.stats(), RunStats::default());
            router.end(Ok(()));
            assert_eq!(next.recv().await.unwrap(), None);
        }
    }
}
