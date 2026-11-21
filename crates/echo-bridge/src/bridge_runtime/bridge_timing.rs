//! The run owns delegated waiting. Keep response production and transport bounded.
use crate::{Error, bridge_input::Settings};
use std::time::Duration;
use tokio::time::Instant;

pub(super) const CONTROL: Duration = Duration::from_secs(5);
const RESPONSE: Duration = Duration::from_secs(10);

pub(super) struct Wait {
    pub cutoff: Instant,
    response: Instant,
    phase: Phase,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Response,
    DelegatedWait,
    DelegatedReply,
}

impl Wait {
    pub fn cutoff(settings: &Settings, opened: Instant) -> Result<Instant, Error> {
        let voice = settings
            .config
            .as_ref()
            .map_or_else(echo_config::VoiceConfig::default, |config| config.voice);
        opened
            .checked_add(Duration::from_secs(u64::from(voice.session_max_seconds)))
            .ok_or(Error::Deadline)
    }

    pub fn new(cutoff: Instant) -> Self {
        Self {
            cutoff,
            response: Instant::now() + RESPONSE,
            phase: Phase::Response,
        }
    }

    pub fn delegation_started(&mut self) {
        self.phase = Phase::DelegatedWait;
    }

    pub fn media_started(&mut self, awaiting_result: bool) {
        // An acknowledgement can straddle the handoff. Only audio after the
        // result has been sent starts the delegated reply's response timer.
        if self.phase == Phase::DelegatedWait && !awaiting_result {
            self.phase = Phase::DelegatedReply;
            self.response = Instant::now() + RESPONSE;
        }
    }

    pub fn next_reply(&mut self) {
        if self.phase != Phase::Response {
            self.response = Instant::now() + RESPONSE;
        }
        self.phase = Phase::Response;
    }

    pub fn deadline(&self, user_speaking: bool) -> (Option<Instant>, Error) {
        if self.phase == Phase::DelegatedWait {
            ((!user_speaking).then_some(self.cutoff), Error::SessionCeiling)
        } else {
            (Some(self.response), Error::Deadline)
        }
    }

    pub fn playback(&self, settings: &Settings) -> Duration {
        // The transport is a backstop. The producer, cut and Played retain
        // their phase bounds, with room for audio accepted near the cutoff.
        let room = if settings.delegation.is_some() || settings.muse.is_some() {
            self.cutoff.saturating_duration_since(Instant::now())
        } else {
            Duration::ZERO
        };
        RESPONSE + room + CONTROL * 3
    }
}
