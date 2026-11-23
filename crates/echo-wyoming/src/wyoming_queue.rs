use crate::{Error, MicFrame, RunStats};
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};
use tokio::sync::Notify;

pub(crate) struct State<T> {
    frames: VecDeque<T>,
    retain: bool,
    muted: bool,
    end_seen: bool,
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
                frames: VecDeque::with_capacity(capacity.min(crate::ClientOptions::default().mic_capacity)),
                retain: true,
                muted: false,
                end_seen: false,
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

    pub(crate) fn mute(&self) {
        let mut state = self.lock();
        state.muted = true;
        state.end.get_or_insert(Ok(()));
        drop(state);
        self.ready.notify_waiters();
    }

    pub(crate) async fn drained(&self) {
        loop {
            let notified = self.ready.notified();
            if self.lock().end_seen {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn use_event_audio(&self) {
        let mut state = self.lock();
        state.frames.clear();
        state.retain = false;
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
        state.end_seen = true;
        drop(state);
        self.ready.notify_waiters();
        count
    }

    pub(crate) async fn recv(&self) -> Result<Option<T>, Error> {
        loop {
            let notified = self.ready.notified();
            {
                let mut state = self.lock();
                if let Some(frame) = state.frames.pop_front() {
                    self.ready.notify_waiters();
                    return Ok(Some(frame));
                }
                if let Some(result) = state.end {
                    state.end_seen = true;
                    self.ready.notify_waiters();
                    return result.map(|()| None);
                }
            }
            notified.await;
        }
    }
}

impl<T> State<T> {
    fn push(&mut self, frame: T, capacity: usize) {
        if self.muted {
            self.stats.received += 1;
            self.stats.dropped += 1;
            return;
        }
        if self.end.is_some() {
            return;
        }
        self.stats.received += 1;
        if !self.retain {
            return;
        }
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

    #[tokio::test(start_paused = true)]
    async fn default_mic_capacity_retains_lead_in_and_two_seconds() {
        let queue = MicQueue::new(crate::ClientOptions::default().mic_capacity);
        for index in 0..53_u8 {
            queue.push(MicFrame {
                data: echo_contracts::wyoming::AudioChunkData {
                    rate: 16000,
                    width: 2,
                    channels: 1,
                    timestamp: None,
                    extra: std::collections::BTreeMap::default(),
                },
                payload: vec![index; if index == 12 { 1024 } else { 2048 }].into(),
            });
        }
        assert_eq!(
            queue.stats(),
            RunStats {
                received: 53,
                dropped: 0
            }
        );
        queue.close(Ok(()));
        for index in 0..53_u8 {
            assert_eq!(
                queue.recv().await.unwrap().unwrap().payload.as_ref(),
                vec![index; if index == 12 { 1024 } else { 2048 }]
            );
        }
        assert_eq!(queue.recv().await.unwrap(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn event_audio_retains_counts_without_duplicate_queue_evictions() {
        let queue = FrameQueue::new(1);
        queue.push(1_u8);
        queue.use_event_audio();
        queue.push(2);
        queue.push(3);
        assert_eq!(
            queue.stats(),
            RunStats {
                received: 3,
                dropped: 0
            }
        );
        queue.close(Ok(()));
        assert_eq!(queue.recv().await, Ok(None));
        queue.push(4);
        assert_eq!(
            queue.stats(),
            RunStats {
                received: 3,
                dropped: 0
            }
        );
    }

    #[test]
    fn loom_queue_close_receive_and_oldest_eviction_conserve_frames() {
        loom::model(|| {
            // Production State and critical sections; loom substitutes only
            // the mutex/scheduler, not the eviction/counting algorithm.
            let state = Arc::new(Mutex::new(State {
                frames: VecDeque::new(),
                retain: true,
                muted: false,
                end_seen: false,
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
