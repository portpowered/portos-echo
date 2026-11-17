// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host incoming.rs; Modified by Port OS.
use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::Notify;

pub(crate) struct Queue<T> {
    inner: Mutex<Inner<T>>,
    pub(crate) ready: Notify,
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
                    return Some(frame);
                }
            }
            ready.await;
        }
    }
    pub(crate) fn close(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.closed = true;
        inner.frames.clear();
        drop(inner);
        self.ready.notify_waiters();
    }
    pub(crate) fn drops(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drops
    }
}
