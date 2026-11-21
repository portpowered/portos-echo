//! Journeys that hold the injected run clock across a long wait: an admitted
//! long turn, and a long question whose hand-off is slow to be answered.
use super::super::{HANDBACK_MS, HANDOFF_MS, LONG_CALLS, LONG_WAIT_MS, QUESTION_MS};
use super::{Control, STALL_MS, World};
use serde_json::json;

/// Where the long question stands.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Question {
    Speaking,
    Delegated,
}

impl World {
    /// Run the journey's long wait when it is due. True when the run clock
    /// moved, so the caller restarts its stall watch.
    pub(super) async fn long_wait(&mut self, admitting: bool) -> Result<bool, String> {
        match self.question {
            Some(Question::Speaking) if admitting => {
                self.ask_long_question().await?;
                self.question = Some(Question::Delegated);
                return Ok(true);
            }
            Some(Question::Delegated) if self.provider()?.count("waiting") > 0 => {
                self.resolve_slow_handoff().await?;
                self.question = None;
                return Ok(true);
            }
            _ => {}
        }
        if !self.wait_for_long_answer {
            return Ok(false);
        }
        self.release_long_answer().await?;
        Ok(!self.wait_for_long_answer)
    }

    /// The user speaks for the whole question before anything is delegated.
    async fn ask_long_question(&mut self) -> Result<(), String> {
        self.speech("speech-started", "input_audio_buffer.speech_started")
            .await?;
        self.advance_live(QUESTION_MS).await?;
        self.speech("speech-stopped", "input_audio_buffer.speech_stopped").await
    }

    /// One Live voice-activity event, delivered on the sideband.
    async fn speech(&mut self, tag: &'static str, kind: &str) -> Result<(), String> {
        let sent = self.provider()?.count("events_sent");
        self.provider()?
            .control(Control::Events(tag, vec![json!({"type":kind})]))
            .await?;
        self.pace("speech event", |world| {
            world
                .provider()
                .is_ok_and(|provider| provider.count("events_sent") > sent)
        })
        .await
    }

    /// The runner's answer is held, then handed back, and the voice still
    /// takes a while to begin it: past one no-answer window from the wake.
    async fn resolve_slow_handoff(&mut self) -> Result<(), String> {
        self.advance_live(HANDOFF_MS).await?;
        self.provider()?.control(Control::ReleaseGoodbye).await?;
        self.pace("hand-back", |world| {
            world.provider().is_ok_and(|provider| provider.count("appends") > 0)
        })
        .await?;
        self.advance_live(HANDBACK_MS).await
    }

    /// Keep the media cadence until the provider reports `done`.
    async fn pace(&mut self, label: &str, done: impl Fn(&Self) -> bool) -> Result<(), String> {
        let started = self.epoch();
        while !done(self) {
            self.provider
                .as_mut()
                .ok_or("session: missing provider")?
                .tick()
                .await?;
            self.step(20).await?;
            if self.epoch() > started + STALL_MS {
                return Err(self.stalled(label));
            }
        }
        Ok(())
    }

    async fn release_long_answer(&mut self) -> Result<(), String> {
        if self.provider()?.count("waiting") == 0 {
            self.read_trace()?;
            if self.trace.records.iter().any(|row| row["event"] == "turn/interrupt") {
                let applied = self
                    .trace
                    .records
                    .iter()
                    .filter(|row| row["event"] == "session.tool_receipt")
                    .count();
                return Err(format!(
                    "session: admitted long turn interrupted after {applied} of {LONG_CALLS} tool effects"
                ));
            }
            return Ok(());
        }
        // The pin has acknowledged all twelve calls and is waiting on its
        // last model response. Advance only the bridge's injected clock.
        self.advance_live(LONG_WAIT_MS).await?;
        self.provider()?.control(Control::ReleaseGoodbye).await?;
        self.wait_for_long_answer = false;
        Ok(())
    }

    /// Advance the injected run clock in increments while the real local
    /// media peer services ICE. A single jump would manufacture consent loss.
    pub(super) async fn advance_live(&mut self, mut remaining: u64) -> Result<(), String> {
        while remaining > 0 {
            let step = remaining.min(500);
            self.provider
                .as_mut()
                .ok_or("session: missing provider")?
                .tick()
                .await?;
            self.step(step).await?;
            remaining -= step;
        }
        Ok(())
    }
}
