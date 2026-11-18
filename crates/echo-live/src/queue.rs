use std::collections::VecDeque;

use crate::Error;

pub(crate) struct Queue<T> {
    items: VecDeque<T>,
    capacity: usize,
    closed: bool,
    pub drops: u64,
}

impl<T> Queue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity,
            closed: false,
            drops: 0,
        }
    }

    pub fn push(&mut self, item: T, drop_oldest: bool) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if self.items.len() == self.capacity {
            if !drop_oldest {
                return Err(Error::Full);
            }
            self.items.pop_front();
            self.drops += 1;
        }
        self.items.push_back(item);
        Ok(())
    }

    #[cfg(test)]
    pub fn items_len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    pub fn items_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn pop(&mut self) -> Option<T> {
        self.items.pop_front()
    }

    pub fn close(&mut self) {
        self.closed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_pressure_drops_exactly_oldest() -> Result<(), Error> {
        let mut queue = Queue::new(128);
        for n in 0..129 {
            queue.push(n, true)?;
        }
        assert_eq!(queue.drops, 1);
        for n in 1..129 {
            assert_eq!(queue.pop(), Some(n));
        }
        assert_eq!(queue.pop(), None);
        queue.close();
        assert_eq!(queue.push(129, true), Err(Error::Closed));
        Ok(())
    }

    #[test]
    fn text_pressure_and_close_preserve_fifo() -> Result<(), Error> {
        let mut queue = Queue::new(16);
        for n in 0..16 {
            queue.push(n, false)?;
        }
        assert_eq!(queue.push(16, false), Err(Error::Full));
        queue.close();
        queue.close();
        assert_eq!(queue.push(17, false), Err(Error::Closed));
        for n in 0..16 {
            assert_eq!(queue.pop(), Some(n));
        }
        assert_eq!(queue.pop(), None);
        Ok(())
    }
}
