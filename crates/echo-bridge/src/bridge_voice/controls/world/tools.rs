use super::slots::Slots;
use echo_core_api::{SessionToolDecision, ToolCallHandle};
use std::collections::VecDeque;

const ANSWERED: usize = 64;

struct Entry {
    key: String,
    decision: Option<SessionToolDecision>,
}

/// One admitted run owns the retention window. Entries keep their original
/// generation until eviction or run release.
pub(super) struct ToolSlots {
    slots: Slots<Entry>,
    fifo: VecDeque<ToolCallHandle>,
}

impl ToolSlots {
    pub(super) fn new() -> Self {
        Self {
            slots: Slots::new(ANSWERED),
            fifo: VecDeque::with_capacity(ANSWERED),
        }
    }

    pub(super) fn replay(&self, key: &str) -> Option<SessionToolDecision> {
        self.fifo
            .iter()
            .filter_map(|tool| self.slots.get(tool.index, tool.generation))
            .find(|entry| entry.key == key)?
            .decision
    }

    pub(super) fn start(&mut self, key: String) -> Option<ToolCallHandle> {
        if self.fifo.len() == ANSWERED
            && let Some(oldest) = self.fifo.pop_front()
        {
            self.remove(oldest);
        }
        let (index, generation) = self.slots.insert(Entry { key, decision: None })?;
        Some(ToolCallHandle { index, generation })
    }

    pub(super) fn retain(&mut self, tool: ToolCallHandle, decision: SessionToolDecision) -> Option<()> {
        self.slots.get_mut(tool.index, tool.generation)?.decision = Some(decision);
        self.fifo.push_back(tool);
        Some(())
    }

    pub(super) fn remove(&mut self, tool: ToolCallHandle) {
        self.slots.remove(tool.index, tool.generation);
    }

    pub(super) fn clear(&mut self) {
        while let Some(tool) = self.fifo.pop_front() {
            self.remove(tool);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.fifo.len()
    }
}
