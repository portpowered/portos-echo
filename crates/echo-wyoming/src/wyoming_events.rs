//! Bounded lifecycle envelopes with ordered, payload-free audio accounting.
use crate::{Error, ReceivedEvent};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::Notify;

struct Entry {
    audio: Option<ReceivedEvent>,
    event: Option<ReceivedEvent>,
}

struct State {
    entries: VecDeque<Entry>,
    trailing: Option<ReceivedEvent>,
    senders: usize,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Notify,
    capacity: usize,
}

pub(crate) struct Sender(Arc<Shared>);
pub(crate) struct Receiver(Arc<Shared>);

pub(crate) fn channel(capacity: usize) -> (Sender, Receiver) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            entries: VecDeque::new(),
            trailing: None,
            senders: 1,
            closed: false,
        }),
        ready: Notify::new(),
        capacity,
    });
    (Sender(shared.clone()), Receiver(shared))
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl State {
    fn push(&mut self, event: ReceivedEvent, capacity: usize) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Capacity);
        }
        if event.audio_count == 0 {
            self.seal(Some(event), capacity)?;
        } else {
            if self
                .trailing
                .as_ref()
                .is_some_and(|audio| !Arc::ptr_eq(&audio.generation, &event.generation))
            {
                // A fresh generation cannot inherit old counts even if its
                // first frame is audio. This marker uses lifecycle capacity.
                self.seal(None, capacity)?;
            }
            if let Some(audio) = &mut self.trailing {
                audio.audio_count = audio
                    .audio_count
                    .checked_add(event.audio_count)
                    .ok_or(Error::Capacity)?;
            } else {
                self.trailing = Some(event);
            }
        }
        Ok(())
    }

    fn seal(&mut self, event: Option<ReceivedEvent>, capacity: usize) -> Result<(), Error> {
        if self.entries.len() == capacity {
            return Err(Error::Capacity);
        }
        self.entries.push_back(Entry {
            audio: self.trailing.take(),
            event,
        });
        Ok(())
    }

    fn pop(&mut self) -> Option<ReceivedEvent> {
        if let Some(entry) = self.entries.front_mut() {
            if let Some(audio) = entry.audio.take() {
                if entry.event.is_none() {
                    self.entries.pop_front();
                }
                return Some(audio);
            }
            return self.entries.pop_front().and_then(|entry| entry.event);
        }
        self.trailing.take()
    }
}

impl Sender {
    pub(crate) fn try_send(&self, event: ReceivedEvent) -> Result<(), Error> {
        self.0.lock().push(event, self.0.capacity)?;
        self.0.ready.notify_one();
        Ok(())
    }
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        self.0.lock().senders += 1;
        Self(self.0.clone())
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.0.lock().senders -= 1;
        self.0.ready.notify_one();
    }
}

impl Receiver {
    pub(crate) async fn recv(&mut self) -> Option<ReceivedEvent> {
        loop {
            tokio::task::consume_budget().await;
            let ready = self.0.ready.notified();
            {
                let mut state = self.0.lock();
                if let Some(event) = state.pop() {
                    return Some(event);
                }
                if state.senders == 0 {
                    return None;
                }
            }
            ready.await;
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.closed = true;
        state.entries.clear();
        state.trailing = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::wyoming::WyomingEvent;
    use serde_json::json;
    use std::{
        future::Future,
        task::{Context, Waker},
    };

    fn event(generation: &Arc<()>, count: u64) -> ReceivedEvent {
        ReceivedEvent {
            generation: generation.clone(),
            audio_count: count,
            event: if count == 0 {
                WyomingEvent::Ping(json!({}))
            } else {
                WyomingEvent::AudioChunk(serde_json::from_value(json!({"rate":16000,"width":2,"channels":1})).unwrap())
            },
            payload: vec![],
            run: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn audio_flood_metadata_is_bounded_counts_do_not_cross_controls_or_generations() {
        let (sender, mut receiver) = channel(2);
        let old = Arc::new(());
        let new = Arc::new(());
        for _ in 0..10_000 {
            sender.try_send(event(&old, 1)).unwrap();
        }
        assert_eq!(sender.0.lock().entries.len(), 0);
        sender.try_send(event(&old, 0)).unwrap();
        sender.try_send(event(&old, 3)).unwrap();
        sender.try_send(event(&new, 7)).unwrap();
        assert_eq!(sender.0.lock().entries.len(), 2);
        assert_eq!(sender.0.lock().trailing.as_ref().unwrap().audio_count, 7);
        drop(sender);
        for (generation, count) in [(&old, 10_000), (&old, 0), (&old, 3), (&new, 7)] {
            let notification = receiver.recv().await.unwrap();
            assert!(Arc::ptr_eq(&notification.generation, generation));
            assert_eq!(notification.audio_count, count);
        }
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn audio_count_overflow_and_control_overflow_preserve_admitted_data() {
        let (sender, mut receiver) = channel(1);
        let generation = Arc::new(());
        sender.try_send(event(&generation, u64::MAX)).unwrap();
        assert_eq!(sender.try_send(event(&generation, 1)), Err(Error::Capacity));
        sender.try_send(event(&generation, 0)).unwrap();
        assert_eq!(sender.try_send(event(&generation, 0)), Err(Error::Capacity));
        drop(sender);
        assert_eq!(receiver.recv().await.unwrap().audio_count, u64::MAX);
        assert_eq!(receiver.recv().await.unwrap().audio_count, 0);
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_receive_retains_audio_and_last_sender_wakes_receiver() {
        let (sender, mut receiver) = channel(1);
        let mut pending = Box::pin(receiver.recv());
        assert!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(pending);
        sender.try_send(event(&Arc::new(()), 5)).unwrap();
        assert_eq!(receiver.recv().await.unwrap().audio_count, 5);
        let other = sender.clone();
        drop(sender);
        let mut pending = Box::pin(receiver.recv());
        assert!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(other);
        assert!(pending.await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_receiver_rejects_control_and_audio() {
        let (sender, receiver) = channel(1);
        drop(receiver);
        for count in [0, 1] {
            assert_eq!(sender.try_send(event(&Arc::new(()), count)), Err(Error::Capacity));
        }
    }

    #[test]
    fn loom_queue_audio_counts_and_close_receive_conserve_admitted_events() {
        loom::model(|| {
            let generation = Arc::new(());
            let state = loom::sync::Arc::new(loom::sync::Mutex::new(State {
                entries: VecDeque::new(),
                trailing: None,
                senders: 1,
                closed: false,
            }));
            let writer_state = state.clone();
            let writer = loom::thread::spawn(move || {
                for count in [1, 2, 0, 4] {
                    writer_state.lock().unwrap().push(event(&generation, count), 1).unwrap();
                }
                writer_state.lock().unwrap().senders -= 1;
            });
            let reader_state = state.clone();
            let reader = loom::thread::spawn(move || reader_state.lock().unwrap().pop());
            writer.join().unwrap();
            let mut delivered = reader.join().unwrap().into_iter().collect::<Vec<_>>();
            let mut state = state.lock().unwrap();
            while let Some(event) = state.pop() {
                delivered.push(event);
            }
            assert_eq!(state.senders, 0);
            assert_eq!(delivered.iter().map(|event| event.audio_count).sum::<u64>(), 7);
            assert_eq!(delivered.iter().filter(|event| event.audio_count == 0).count(), 1);
            let boundary = delivered.iter().position(|event| event.audio_count == 0).unwrap();
            assert_eq!(
                delivered[..boundary].iter().map(|event| event.audio_count).sum::<u64>(),
                3
            );
            assert_eq!(
                delivered[boundary + 1..]
                    .iter()
                    .map(|event| event.audio_count)
                    .sum::<u64>(),
                4
            );
        });
    }
}
