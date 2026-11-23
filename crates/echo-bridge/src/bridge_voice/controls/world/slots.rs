use std::num::NonZeroU64;

struct Slot<T> {
    generation: NonZeroU64,
    value: Option<T>,
}

pub(super) struct Slots<T> {
    entries: Vec<Slot<T>>,
    free: Vec<usize>,
}

impl<T> Slots<T> {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            entries: (0..capacity)
                .map(|_| Slot {
                    generation: NonZeroU64::MIN,
                    value: None,
                })
                .collect(),
            free: (0..capacity).rev().collect(),
        }
    }

    pub(super) fn insert(&mut self, value: T) -> Option<(usize, NonZeroU64)> {
        let index = self.free.pop()?;
        let slot = self.entries.get_mut(index)?;
        slot.value = Some(value);
        Some((index, slot.generation))
    }

    pub(super) fn get(&self, index: usize, generation: NonZeroU64) -> Option<&T> {
        let slot = self.entries.get(index)?;
        (slot.generation == generation).then_some(slot.value.as_ref()).flatten()
    }

    pub(super) fn remove(&mut self, index: usize, generation: NonZeroU64) -> Option<T> {
        let slot = self.entries.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        // Never allow a wrapped generation to stand for an earlier handle.
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(index);
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_core_api::ToolHandle;
    use std::io::Write;

    #[test]
    fn session_slots_reject_stale_tool_handle_after_reuse() {
        let mut slots = Slots::new(1);
        let (index, generation) = slots.insert("first").unwrap();
        let first = ToolHandle { index, generation };
        assert_eq!(slots.remove(first.index, first.generation), Some("first"));
        let (index, generation) = slots.insert("second").unwrap();
        let second = ToolHandle { index, generation };
        assert_eq!(first.index, second.index);
        assert_ne!(first.generation, second.generation);
        assert_eq!(slots.get(first.index, first.generation), None);
        assert_eq!(slots.remove(first.index, first.generation), None);
        assert_eq!(slots.get(second.index, second.generation), Some(&"second"));
        writeln!(
            std::io::stdout(),
            "slot {index}: generation {} rejected after reuse as generation {generation}",
            first.generation
        )
        .unwrap();
    }

    #[test]
    fn session_slots_full_admission_preserves_occupied_slots() {
        let mut slots = Slots::new(2);
        let (index, generation) = slots.insert("first").unwrap();
        let (other, current) = slots.insert("second").unwrap();
        assert_eq!(slots.insert("excess"), None);
        assert_eq!(slots.get(index, generation), Some(&"first"));
        assert_eq!(slots.get(other, current), Some(&"second"));
    }

    #[test]
    fn session_slots_exhausted_generation_retires_the_slot() {
        let mut slots = Slots::new(1);
        slots.entries[0].generation = NonZeroU64::MAX;
        let (index, generation) = slots.insert(17).unwrap();
        assert_eq!(slots.remove(index, generation), Some(17));
        assert_eq!(slots.insert(19), None);
        assert_eq!(slots.get(index, generation), None);
    }
}
