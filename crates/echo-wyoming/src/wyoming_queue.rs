use crate::{Error, MicFrame, RunStats};
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};
use tokio::sync::Notify;

struct State {
    frames: VecDeque<MicFrame>,
    stats: RunStats,
    end: Option<Result<(), Error>>,
}

pub(crate) struct MicQueue {
    state: Mutex<State>,
    capacity: usize,
    ready: Notify,
}

impl MicQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                frames: VecDeque::new(),
                stats: RunStats::default(),
                end: None,
            }),
            capacity,
            ready: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn push(&self, frame: MicFrame) {
        let mut state = self.lock();
        if state.end.is_some() {
            return;
        }
        state.stats.received += 1;
        if state.frames.len() == self.capacity {
            state.frames.pop_front();
            state.stats.dropped += 1;
        }
        state.frames.push_back(frame);
        drop(state);
        self.ready.notify_one();
    }

    pub(crate) fn close(&self, result: Result<(), Error>) {
        self.lock().end.get_or_insert(result);
        self.ready.notify_one();
    }

    pub(crate) fn stats(&self) -> RunStats {
        self.lock().stats
    }

    pub(crate) async fn recv(&self) -> Result<Option<MicFrame>, Error> {
        loop {
            let notified = self.ready.notified();
            {
                let mut state = self.lock();
                if let Some(frame) = state.frames.pop_front() {
                    return Ok(Some(frame));
                }
                if let Some(result) = state.end {
                    return result.map(|()| None);
                }
            }
            notified.await;
        }
    }
}
