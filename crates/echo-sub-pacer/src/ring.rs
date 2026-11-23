use super::REPLY_CAPACITY;
use echo_core_api::ReplyFrame;

pub(crate) struct Ring {
    slots: Box<[ReplyFrame]>,
    head: usize,
    len: usize,
    dropped: u64,
}

impl Default for Ring {
    fn default() -> Self {
        Self {
            slots: vec![[0; 960]; REPLY_CAPACITY].into_boxed_slice(),
            head: 0,
            len: 0,
            dropped: 0,
        }
    }
}

impl Ring {
    pub(crate) fn push(&mut self, frame: &ReplyFrame) {
        self.slots[(self.head + self.len) % REPLY_CAPACITY] = *frame;
        if self.len == REPLY_CAPACITY {
            self.advance();
            self.dropped += 1;
        } else {
            self.len += 1;
        }
    }

    pub(crate) fn pop(&mut self) -> Option<ReplyFrame> {
        if self.len == 0 {
            return None;
        }
        let frame = self.slots[self.head];
        self.advance();
        self.len -= 1;
        Some(frame)
    }

    fn advance(&mut self) {
        self.head = (self.head + 1) % REPLY_CAPACITY;
    }

    pub(crate) fn clear(&mut self) {
        self.discard();
        self.dropped = 0;
    }

    pub(crate) fn discard(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    #[cfg(test)]
    pub(crate) fn storage(&self) -> (usize, *const ReplyFrame) {
        (self.slots.len(), self.slots.as_ptr())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_depth(&mut self, depth: usize) {
        self.len = depth;
    }
}
