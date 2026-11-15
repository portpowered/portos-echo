use crate::{Error, MicFrame, ReceivedEvent, Run, wyoming_queue::MicQueue};
use echo_contracts::wyoming::{Event, WyomingEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) struct Router {
    pub(crate) current: Option<Arc<MicQueue>>,
    pub(crate) events: mpsc::Sender<ReceivedEvent>,
    pub(crate) capacity: usize,
}

impl Router {
    pub(crate) fn route(&mut self, frame: Event) -> Result<WyomingEvent, Error> {
        let event = frame.typed().map_err(|_| Error::Protocol)?;
        let mut run = None;
        match &event {
            WyomingEvent::RunPipeline(pipeline) => {
                self.end(Ok(()));
                let queue = Arc::new(MicQueue::new(self.capacity));
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
                        payload: frame.payload.clone(),
                    });
                }
            }
            WyomingEvent::AudioStop(_) => self.end(Ok(())),
            _ => {}
        }
        self.events
            .try_send(ReceivedEvent {
                event: event.clone(),
                payload: frame.payload,
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
