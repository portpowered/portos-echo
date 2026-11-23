use std::{
    fmt,
    ops::Deref,
    sync::{Arc, Mutex, PoisonError, Weak},
};

type Slots = Mutex<Vec<Vec<u8>>>;

/// Reusable capture slots. A frame retains exclusive ownership of its bytes
/// until its consumer drops it, even after its run or connection has ended.
pub struct MicPool {
    slots: Arc<Slots>,
    slot_bytes: usize,
}

impl MicPool {
    #[must_use]
    pub fn new(count: usize, slot_bytes: usize) -> Self {
        let slots = (0..count).map(|_| Vec::with_capacity(slot_bytes)).collect();
        Self {
            slots: Arc::new(Mutex::new(slots)),
            slot_bytes,
        }
    }

    /// Lease a slot or fall back to owned storage when the sample exceeds a
    /// slot or consumers are still holding all the slots.
    #[must_use]
    pub fn copy(&self, bytes: &[u8]) -> MicPayload {
        if bytes.len() <= self.slot_bytes
            && let Some(mut buffer) = self.slots.lock().unwrap_or_else(PoisonError::into_inner).pop()
        {
            buffer.extend_from_slice(bytes);
            return MicPayload {
                buffer,
                slots: Arc::downgrade(&self.slots),
            };
        }
        bytes.to_vec().into()
    }
}

/// The logical bytes of one microphone frame.
#[derive(Default)]
pub struct MicPayload {
    buffer: Vec<u8>,
    slots: Weak<Slots>,
}

impl From<Vec<u8>> for MicPayload {
    fn from(buffer: Vec<u8>) -> Self {
        Self {
            buffer,
            slots: Weak::new(),
        }
    }
}

impl AsRef<[u8]> for MicPayload {
    fn as_ref(&self) -> &[u8] {
        &self.buffer
    }
}

impl Deref for MicPayload {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl fmt::Debug for MicPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.buffer.fmt(f)
    }
}

impl PartialEq for MicPayload {
    fn eq(&self, other: &Self) -> bool {
        self.buffer == other.buffer
    }
}

impl Eq for MicPayload {}

impl Drop for MicPayload {
    fn drop(&mut self) {
        if let Some(slots) = self.slots.upgrade() {
            self.buffer.clear();
            slots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(std::mem::take(&mut self.buffer));
        }
    }
}
