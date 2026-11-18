//! Run lifecycle decisions, independent of PCM conversion and device IO.
use super::Error;

#[derive(Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Listening,
    Replying,
    Cutting,
    Retired,
}

/// The preserving extraction retains the one-reply completion boundary.
/// A cut acknowledgment belongs to the retired playback, even when its ordered
/// device event arrives after the replacement playback has started.
#[derive(Debug, Default)]
pub(super) struct Controller {
    state: State,
    played: bool,
    cut_receipts: usize,
    microphone_finished: bool,
}

impl Controller {
    pub fn speaking(&self) -> bool {
        self.state == State::Replying
    }

    pub fn cutting(&self) -> bool {
        self.state == State::Cutting
    }

    pub fn retired(&self) -> bool {
        self.state == State::Retired
    }

    pub fn interruptible(&self, delegated: bool) -> bool {
        self.speaking() || (self.state == State::Listening && delegated)
    }

    pub fn accepts_audio(&self) -> bool {
        !matches!(self.state, State::Cutting | State::Retired)
    }

    pub fn reply_started(&mut self) {
        self.state = State::Replying;
    }

    pub fn cut(&mut self) {
        self.state = State::Cutting;
    }

    pub fn cut_settled(&mut self) {
        self.state = State::Retired;
        self.cut_receipts += usize::from(!self.played);
        self.played = false;
    }

    /// False consumes a delayed cut receipt; true acknowledges this reply.
    pub fn played(&mut self, stopped: bool) -> Result<bool, Error> {
        if self.cut_receipts > 0 {
            self.cut_receipts -= 1;
            return Ok(false);
        }
        if !stopped {
            return Err(Error::Wyoming);
        }
        self.played = true;
        Ok(true)
    }

    pub fn microphone_ended(&mut self) {
        self.microphone_finished = true;
    }

    pub fn microphone_finished(&self) -> bool {
        self.microphone_finished
    }

    pub fn has_played(&self) -> bool {
        self.played
    }

    pub fn pending_cut_receipts(&self) -> usize {
        self.cut_receipts
    }

    pub fn complete(&self, stopped: bool, mouth_drained: bool) -> bool {
        !self.cutting() && self.played && self.microphone_finished && stopped && mouth_drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_controller_requires_played_capture_eof_stop_and_mouth_drain() {
        let mut controller = Controller::default();
        controller.reply_started();
        assert!(!controller.complete(true, true));
        assert_eq!(controller.played(false), Err(Error::Wyoming));
        assert!(!controller.has_played());
        assert_eq!(controller.played(true), Ok(true));
        assert!(!controller.complete(true, true));
        controller.microphone_ended();
        assert!(!controller.complete(false, true));
        assert!(!controller.complete(true, false));
        assert!(controller.complete(true, true));
    }

    #[test]
    fn session_controller_late_cut_receipt_cannot_complete_replacement() {
        let mut controller = Controller::default();
        controller.reply_started();
        controller.cut();
        assert!(!controller.accepts_audio());
        controller.cut_settled();
        assert!(controller.retired());
        assert!(!controller.accepts_audio());
        controller.reply_started();
        controller.microphone_ended();
        assert_eq!(controller.played(false), Ok(false));
        assert!(!controller.complete(true, true));
        assert_eq!(controller.played(false), Err(Error::Wyoming));
        assert_eq!(controller.played(true), Ok(true));
        assert!(controller.complete(true, true));
    }

    #[test]
    fn session_controller_cut_after_played_does_not_steal_replacement_receipt() {
        let mut controller = Controller::default();
        controller.reply_started();
        assert_eq!(controller.played(true), Ok(true));
        controller.cut();
        assert!(!controller.complete(true, true));
        controller.cut_settled();
        assert_eq!(controller.pending_cut_receipts(), 0);
        assert!(!controller.has_played());
        controller.reply_started();
        assert_eq!(controller.played(true), Ok(true));
    }

    #[test]
    fn session_controller_interrupts_only_reply_or_initial_delegated_wait() {
        let mut controller = Controller::default();
        assert!(controller.accepts_audio());
        assert!(!controller.interruptible(false));
        assert!(controller.interruptible(true));
        controller.reply_started();
        assert!(controller.interruptible(false));
        controller.cut();
        assert!(!controller.interruptible(true));
        controller.cut_settled();
        assert!(!controller.interruptible(true));
        controller.reply_started();
        assert!(controller.interruptible(false));
    }
}
