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
        let mut queue = Queue::new(3);
        for n in 0..4 {
            queue.push(n, true)?;
        }
        assert_eq!(queue.drops, 1);
        assert_eq!(
            (queue.pop(), queue.pop(), queue.pop(), queue.pop()),
            (Some(1), Some(2), Some(3), None)
        );
        Ok(())
    }

    #[test]
    fn text_pressure_and_close_preserve_fifo() -> Result<(), Error> {
        let mut queue = Queue::new(2);
        queue.push("first", false)?;
        queue.push("second", false)?;
        assert_eq!(queue.push("third", false), Err(Error::Full));
        assert_eq!(
            (queue.pop(), queue.pop(), queue.pop()),
            (Some("first"), Some("second"), None)
        );
        queue.close();
        queue.close();
        assert_eq!(queue.push("closed", false), Err(Error::Closed));
        Ok(())
    }
}
