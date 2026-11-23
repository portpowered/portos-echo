//! Nonwaiting adapter for the screen's existing mouth-sample contract.
use echo_core_api::{PacerLipBatch, PacerVowel};
use echo_screen_contract::mouth::{MouthBatch, MouthSample, MouthVowel};
use echo_screen_server::MouthSender;
use std::time::Duration;

/// Where batches go. Neither call may wait.
pub(crate) trait LipSink: Send {
    /// Some screen asked for lip sync; none when that is unknown this instant.
    fn wanted(&self) -> Option<bool>;
    /// How many screens took the batch.
    fn send(&self, batch: &MouthBatch) -> usize;
}

impl LipSink for MouthSender {
    fn wanted(&self) -> Option<bool> {
        MouthSender::wanted(self)
    }

    fn send(&self, batch: &MouthBatch) -> usize {
        MouthSender::send(self, batch)
    }
}

pub(crate) struct Lips {
    sink: Box<dyn LipSink>,
    delay: Duration,
}

impl Lips {
    pub(crate) fn new(sink: Box<dyn LipSink>, delay: Duration) -> Self {
        Self { sink, delay }
    }

    pub(crate) fn delay(&self) -> Duration {
        self.delay
    }

    pub(crate) fn wanted(&self) -> Option<bool> {
        self.sink.wanted()
    }

    pub(crate) fn send(&self, batch: &PacerLipBatch) -> bool {
        let samples = batch
            .samples
            .iter()
            .flatten()
            .map(|sample| MouthSample {
                at_ms: sample.at_ms,
                open: sample.open,
                vowel: match sample.vowel {
                    PacerVowel::None => MouthVowel::None,
                    PacerVowel::A => MouthVowel::A,
                    PacerVowel::I => MouthVowel::I,
                    PacerVowel::U => MouthVowel::U,
                    PacerVowel::E => MouthVowel::E,
                    PacerVowel::O => MouthVowel::O,
                },
            })
            .collect();
        self.sink.send(&MouthBatch {
            sent_ms: batch.sent_ms,
            samples,
        }) != 0
    }
}

#[cfg(test)]
#[path = "reply_lips_tests.rs"]
mod tests;
