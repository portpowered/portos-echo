//! The run owns delegated waiting. Keep response production and transport bounded.
use crate::{Error, bridge_input::Settings};
use std::time::Duration;
use tokio::time::Instant;

pub(super) const CONTROL: Duration = Duration::from_secs(5);
const RESPONSE: Duration = Duration::from_secs(10);

pub(super) struct Wait {
    pub cutoff: Instant,
    response: Instant,
    media_deadline: Option<Instant>,
    phase: Phase,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Response,
    DelegatedWait,
    DelegatedReply,
    Draining,
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
            media_deadline: None,
            phase: Phase::Response,
        }
    }

    pub fn delegation_started(&mut self) {
        self.phase = Phase::DelegatedWait;
    }

    pub fn media_received(&mut self, awaiting_result: bool) {
        let now = Instant::now();
        self.media_deadline = Some(now + RESPONSE);
        // An acknowledgement can straddle the handoff. Only audio after the
        // result has been sent starts the delegated reply's response timer.
        if self.phase == Phase::DelegatedWait && !awaiting_result {
            self.phase = Phase::DelegatedReply;
            self.response = now + RESPONSE;
        }
    }

    pub fn next_reply(&mut self) {
        if self.phase != Phase::Response {
            self.response = Instant::now() + RESPONSE;
        }
        self.phase = Phase::Response;
        self.media_deadline = None;
    }

    pub fn delegation_cutoff(&self) -> Option<Instant> {
        (self.phase == Phase::DelegatedWait).then_some(self.cutoff)
    }

    pub fn draining(&self) -> bool {
        self.phase == Phase::Draining
    }

    pub fn drain_media(&mut self, buffered: bool) -> bool {
        let now = Instant::now();
        // Earlier acknowledgement PCM can have already played during a long
        // wait. Only buffered or recent audio needs the response drain window.
        if buffered || self.media_deadline.is_some_and(|deadline| now < deadline) {
            self.start_drain(now);
            true
        } else {
            false
        }
    }

    pub fn start_drain(&mut self, now: Instant) {
        self.phase = Phase::Draining;
        self.response = now + RESPONSE;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bridge_timing_stale_acknowledgement_uses_the_run_ceiling() {
        let cutoff = Instant::now() + Duration::from_secs(90);
        let mut wait = Wait::new(cutoff);
        wait.media_received(false);
        wait.delegation_started();
        tokio::time::advance(Duration::from_secs(90)).await;
        assert_eq!(wait.deadline(true), (None, Error::SessionCeiling));
        assert_eq!(wait.deadline(false), (Some(cutoff), Error::SessionCeiling));
        assert!(!wait.drain_media(false));
        assert!(wait.drain_media(true));
        assert_eq!(wait.deadline(false), (Some(cutoff + RESPONSE), Error::Deadline));
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_timing_active_acknowledgement_drain_keeps_a_fixed_response_deadline() {
        let cutoff = Instant::now() + Duration::from_secs(90);
        let mut wait = Wait::new(cutoff);
        wait.delegation_started();
        tokio::time::advance(Duration::from_secs(89)).await;
        wait.media_received(true);
        assert_eq!(wait.deadline(false), (Some(cutoff), Error::SessionCeiling));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(wait.drain_media(false));
        tokio::time::advance(Duration::from_secs(1)).await;
        wait.media_received(true);
        assert_eq!(wait.deadline(false), (Some(cutoff + RESPONSE), Error::Deadline));
    }
}
