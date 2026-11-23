//! Execute the session reducer's barge and latch observations at the media fences.
use super::{Error, Source, VoicePort};
use crate::bridge_runtime::bridge_pacing::Cut;
#[cfg(test)]
use echo_core_api::BargeTrigger;
use echo_core_api::{SessionCommand, SessionOutcome, SessionTurnEvent};
use tokio::time::Instant;

impl Source {
    pub fn speech_started(&mut self) {
        let outcome = self.controller.turn(SessionTurnEvent::SpeechStarted).outcome;
        if outcome == Some(Ok(SessionOutcome::Held)) {
            self.meter.held(Instant::now());
        }
    }

    pub fn user_transcript(&mut self, text: String, whole: bool) -> String {
        let (text, held) = self
            .controller
            .transcript(text, whole, self.reply.sounded(), self.reply.voiced());
        if held {
            self.meter.held(Instant::now());
        }
        text
    }

    #[cfg(test)]
    pub fn heard_user(&mut self, trigger: BargeTrigger, said: &str, whole: bool) {
        match trigger {
            BargeTrigger::UserTranscript => {
                self.user_transcript(said.into(), whole);
            }
            BargeTrigger::SpeechStarted => self.speech_started(),
        }
    }

    pub fn user_quiet(&self) {
        self.controller.turn(SessionTurnEvent::SpeechStopped {
            voiced: self.reply.voiced(),
        });
    }

    pub fn barge_due(&self) -> Option<Instant> {
        self.controller.barge_due(self.reply.finished())
    }

    /// Retire playback at its existing service fence and discard its abandoned
    /// tail. Stream order identifies the replacement's next assistant delta.
    pub fn interrupt(&mut self, port: &mut impl VoicePort) -> Result<(), Error> {
        let out = self.controller.turn(SessionTurnEvent::BargeDue {
            finished: self.reply.finished(),
        });
        let Some(SessionCommand::CutReply { trigger, .. }) = out.effect else {
            return Ok(());
        };
        let (silent_ms, yield_ms) = self.meter.cut(trigger);
        self.stats.cut = Some(Box::new(Cut {
            text: out.cut_text,
            silent_ms,
            yield_ms,
        }));
        port.retire_audio();
        self.fresh_reply(true)
    }
}
