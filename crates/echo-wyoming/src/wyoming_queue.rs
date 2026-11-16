use crate::{Error, MicFrame, RunStats};
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};
use tokio::sync::Notify;

pub(crate) struct State<T> {
    frames: VecDeque<T>,
    stats: RunStats,
    end: Option<Result<(), Error>>,
}

pub(crate) type MicQueue = FrameQueue<MicFrame>;

pub(crate) struct FrameQueue<T> {
    state: Mutex<State<T>>,
    capacity: usize,
    ready: Notify,
}

impl<T> FrameQueue<T> {
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

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn push(&self, frame: T) {
        let mut state = self.lock();
        state.push(frame, self.capacity);
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

    pub(crate) fn failure(&self) -> Option<Error> {
        self.lock().end.and_then(Result::err)
    }

    pub(crate) fn discard(&self) -> u64 {
        let mut state = self.lock();
        let count = state.frames.len() as u64;
        state.frames.clear();
        state.end = Some(Ok(()));
        drop(state);
        self.ready.notify_one();
        count
    }

    pub(crate) async fn recv(&self) -> Result<Option<T>, Error> {
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

impl<T> State<T> {
    fn push(&mut self, frame: T, capacity: usize) {
        if self.end.is_some() {
            return;
        }
        self.stats.received += 1;
        if self.frames.len() == capacity {
            self.frames.pop_front();
            self.stats.dropped += 1;
        }
        self.frames.push_back(frame);
    }
}

#[cfg(test)]
mod wyoming_loom {
    use super::*;
    use loom::{sync::Arc, sync::Mutex, thread};

    #[test]
    fn loom_queue_close_receive_and_oldest_eviction_conserve_frames() {
        loom::model(|| {
            // Production State and critical sections; loom substitutes only
            // the mutex/scheduler, not the eviction/counting algorithm.
            let state = Arc::new(Mutex::new(State {
                frames: VecDeque::new(),
                stats: RunStats::default(),
                end: None,
            }));
            let producer = state.clone();
            let writer = thread::spawn(move || {
                for frame in 1..=3 {
                    producer.lock().unwrap().push(frame, 1);
                }
            });
            let consumer = state.clone();
            let reader = thread::spawn(move || {
                let frame = consumer.lock().unwrap().frames.pop_front();
                consumer.lock().unwrap().end.get_or_insert(Ok(()));
                frame
            });
            writer.join().unwrap();
            let delivered = u64::from(reader.join().unwrap().is_some());
            let state = state.lock().unwrap();
            assert!(state.frames.len() <= 1);
            assert_eq!(
                state.stats.received,
                delivered + state.stats.dropped + state.frames.len() as u64
            );
            assert_eq!(state.end, Some(Ok(())));
            assert!(state.stats.received <= 3);
        });
    }
}
