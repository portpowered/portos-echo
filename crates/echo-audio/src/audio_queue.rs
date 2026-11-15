use std::{collections::VecDeque, num::NonZeroUsize};

/// Single-owner FIFO with a fixed positive entry bound. Synchronize shared
/// callers externally; never hold their lock across a pacer await.
/// Caller-owned T allocations are outside this entry bound.
pub struct FrameQueue<T> {
    frames: VecDeque<T>,
    capacity: NonZeroUsize,
    drops: u64,
}

impl<T> FrameQueue<T> {
    /// Zero capacity is unrepresentable. Allocation is reserved once.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity.get()),
            capacity,
            drops: 0,
        }
    }

    /// Evict and return the oldest entry before inserting into a full queue.
    pub fn push(&mut self, frame: T) -> Option<T> {
        let evicted = if self.frames.len() == self.capacity.get() {
            self.drops = self.drops.saturating_add(1);
            self.frames.pop_front()
        } else {
            None
        };
        self.frames.push_back(frame);
        evicted
    }

    /// Remove the oldest retained entry.
    pub fn pop(&mut self) -> Option<T> {
        self.frames.pop_front()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// Cumulative evictions since construction or clear, saturating at `u64::MAX`.
    #[must_use]
    pub fn drop_count(&self) -> u64 {
        self.drops
    }

    /// Discard old-stream entries and reset drop accounting, retaining capacity.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.drops = 0;
    }
}

#[cfg(test)]
mod audio_queue_tests;
