use crate::wyoming_events;
use crate::{Error, MicFrame, ReceivedEvent, Run, wyoming_queue::MicQueue};
use echo_contracts::wyoming::{Event, WyomingEvent};
use std::sync::Arc;

pub(crate) struct Router {
    pub(crate) generation: Arc<()>,
    pub(crate) current: Option<Arc<MicQueue>>,
    pub(crate) events: wyoming_events::Sender,
    pub(crate) capacity: usize,
    pub(crate) muted: bool,
}

impl Router {
    pub(crate) fn route(&mut self, mut frame: Event) -> Result<WyomingEvent, Error> {
        let event = frame.typed().map_err(|_| Error::Protocol)?;
        let mut run = None;
        let audio_count = u64::from(matches!(event, WyomingEvent::AudioChunk(_)));
        match &event {
            WyomingEvent::RunPipeline(pipeline) => {
                self.end(Ok(()));
                let queue = Arc::new(MicQueue::new(self.capacity));
                if self.muted {
                    queue.mute();
                }
                self.current = Some(queue.clone());
                run = Some(Run {
                    pipeline: pipeline.clone(),
                    queue,
                });
            }
            WyomingEvent::AudioChunk(data) => {
                if let Some(queue) = &self.current {
                    queue.push(MicFrame {
                        data: data.clone(),
                        payload: std::mem::take(&mut frame.payload),
                    });
                }
            }
            WyomingEvent::Other(name, data) if name == "hassmic-mute" => {
                let muted = data["muted"].as_bool().ok_or(Error::Protocol)?;
                self.muted = muted;
                if muted && let Some(queue) = &self.current {
                    queue.mute();
                }
            }
            WyomingEvent::AudioStop(_) => self.end(Ok(())),
            _ => {}
        }
        self.events
            .try_send(ReceivedEvent {
                generation: self.generation.clone(),
                audio_count,
                event: event.clone(),
                payload: if audio_count == 0 { frame.payload } else { vec![] },
                run,
            })
            .map_err(|_| Error::Capacity)?;
        Ok(event)
    }

    pub(crate) fn end(&mut self, result: Result<(), Error>) {
        if let Some(queue) = self.current.take() {
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
        (
            Router {
                generation: Arc::new(()),
                current: None,
                events,
                capacity: 2,
                muted: false,
            },
            receiver,
        )
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
