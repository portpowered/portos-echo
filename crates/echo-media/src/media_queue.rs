// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host incoming.rs; Modified by Port OS.
use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::Notify;

pub(crate) struct Queue<T> {
    inner: Mutex<Inner<T>>,
    pub(crate) ready: Notify,
    space: Notify,
    capacity: usize,
}
struct Inner<T> {
    frames: VecDeque<T>,
    drops: u64,
    closed: bool,
}

impl<T> Queue<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                frames: VecDeque::new(),
                drops: 0,
                closed: false,
            }),
            ready: Notify::new(),
            space: Notify::new(),
            capacity,
        }
    }
    pub(crate) fn push(&self, frame: T) {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.closed {
            return;
        }
        if inner.frames.len() == self.capacity {
            inner.frames.pop_front();
            inner.drops = inner.drops.saturating_add(1);
        }
        inner.frames.push_back(frame);
        drop(inner);
        self.ready.notify_one();
    }
    /// Queue the frame unless the queue is full; false leaves it with the caller.
    /// A closed queue accepts and discards, exactly as `push` does.
    pub(crate) fn try_push(&self, frame: T) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.closed {
            return true;
        }
        if inner.frames.len() == self.capacity {
            return false;
        }
        inner.frames.push_back(frame);
        drop(inner);
        self.ready.notify_one();
        true
    }
    /// Resolve once `try_push` can accept a frame, or the queue closed.
    pub(crate) async fn vacant(&self) {
        loop {
            let space = self.space.notified();
            let mut space = std::pin::pin!(space);
            space.as_mut().enable();
            {
                let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if inner.closed || inner.frames.len() < self.capacity {
                    return;
                }
            }
            space.await;
        }
    }
    pub(crate) async fn pop(&self) -> Option<T> {
        loop {
            let ready = self.ready.notified();
            let mut ready = std::pin::pin!(ready);
            ready.as_mut().enable();
            {
                let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if inner.closed {
                    return None;
                }
                if let Some(frame) = inner.frames.pop_front() {
                    drop(inner);
                    self.space.notify_waiters();
                    return Some(frame);
                }
            }
            ready.await;
        }
    }
    pub(crate) fn discard(&self) {
        self.discard_with(|| {});
    }
    pub(crate) fn discard_with(&self, applied: impl FnOnce()) {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.frames.clear();
        // Serialize ingress admission with acknowledgement of the boundary.
        applied();
        drop(inner);
        self.space.notify_waiters();
    }
    pub(crate) fn close(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.closed = true;
        inner.frames.clear();
        drop(inner);
        self.ready.notify_waiters();
        self.space.notify_waiters();
    }
    pub(crate) fn drops(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drops
    }
}
