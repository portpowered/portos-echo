//! The run owns delegated waiting. Keep response production and transport bounded.
use crate::{Error, bridge_input::Settings};
use std::time::Duration;
use tokio::time::Instant;

pub(super) const CONTROL: Duration = Duration::from_secs(5);
const RESPONSE: Duration = Duration::from_secs(10);

pub(super) struct Wait {
    pub cutoff: Instant,
    response: Instant,
    pub delegated: bool,
    media: bool,
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
            delegated: false,
            media: false,
        }
    }

    pub fn media_started(&mut self) {
        if !self.media && self.delegated {
            self.response = Instant::now() + RESPONSE;
        }
        self.media = true;
    }

    pub fn deadline(&self, user_speaking: bool, draining: bool) -> Result<Option<Instant>, Error> {
        if draining || self.delegated && !self.media && user_speaking {
            return Ok(None);
        }
        let waiting = self.delegated && !self.media;
        let deadline = if waiting { self.cutoff } else { self.response };
        if Instant::now() >= deadline {
            return Err(if waiting {
                Error::SessionCeiling
            } else {
                Error::Deadline
            });
        }
        Ok(Some(deadline))
    }

    pub fn playback(&self) -> Duration {
        // The transport is a backstop. The producer, cut and Played retain
        // their phase bounds, with room for audio accepted near the cutoff.
        let room = self.cutoff.saturating_duration_since(Instant::now());
        RESPONSE + room + CONTROL * 3
    }
}
