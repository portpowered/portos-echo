//! A barge-in silences the reply at once and cuts it once the model yields.
use std::time::Duration;
use tokio::time::Instant;

/// How long a sign of user speech from the Live session keeps a barge-in
/// pending. Each further sign renews it.
pub(crate) const BARGE_WINDOW: Duration = Duration::from_secs(2);
/// How long speech heard at the microphone does. It is the earlier sign and
/// the weaker one: a cough holds the reply this long, and a sentence is
/// taken up by the session's transcript before it runs out.
pub(crate) const MIC_WINDOW: Duration = Duration::from_millis(800);
/// A pending barge-in fires once the reply has gone this long without
/// audible audio from the model: it stopped talking, so the user has the floor.
pub(crate) const BARGE_QUIET: Duration = Duration::from_millis(500);

/// The sign of user speech that armed a barge-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BargeTrigger {
    /// A user-role transcript fragment from the Live session.
    UserTranscript,
    /// The Live session's speech-start item.
    SpeechStarted,
    /// Speech at the microphone that the reply's echo does not explain.
    MicSpeech,
}

impl BargeTrigger {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::UserTranscript => "user_transcript",
            Self::SpeechStarted => "speech_started",
            Self::MicSpeech => "mic_speech",
        }
    }

    fn window(self) -> Duration {
        match self {
            Self::MicSpeech => MIC_WINDOW,
            Self::UserTranscript | Self::SpeechStarted => BARGE_WINDOW,
        }
    }
}

#[derive(Clone, Copy)]
struct Armed {
    trigger: BargeTrigger,
    /// A sign from the Live session has joined the microphone's.
    backed: bool,
    since: Instant,
    until: Instant,
    quiet_since: Instant,
}

/// The Live session transcribes whatever the microphone hears while a reply
/// plays: the user, other voices in the room, the Echo's own speaker. The
/// model decides for itself whether to yield, and the pinned session has no
/// request that makes it. A sign of user speech therefore arms a barge-in:
/// first speech heard at the microphone (`Ear`), then the session's
/// transcript of it, which comes some hundred milliseconds later.
/// While it is pending the reply is held: nothing more is written to the
/// device, so the user hears it stop, and the model's audio waits in the
/// reply queue. It fires when the model's reply audio then stays quiet for
/// `BARGE_QUIET`, counted from the sign or from the last audible frame after
/// it: the reply is cut and the held audio dropped. A model still talking
/// when `BARGE_WINDOW` has passed was not interrupted: the barge-in is
/// dropped and the held audio plays, late but whole.
/// A model that has ended its turn has said so itself: there is no stretch to
/// wait out, and a pending barge-in fires at once.
#[derive(Default)]
pub(crate) struct Barge {
    armed: Option<Armed>,
}

impl Barge {
    /// A sign of user speech came at `now` during a reply. True when it
    /// armed a barge-in, false when it renewed the pending one.
    pub(crate) fn arm(&mut self, now: Instant, trigger: BargeTrigger) -> bool {
        let until = now + trigger.window();
        if let Some(armed) = &mut self.armed {
            armed.until = armed.until.max(until);
            armed.backed |= trigger != BargeTrigger::MicSpeech;
            return false;
        }
        self.armed = Some(Armed {
            trigger,
            backed: trigger != BargeTrigger::MicSpeech,
            since: now,
            until,
            quiet_since: now,
        });
        true
    }

    /// When the microphone armed the pending barge-in, once the Live
    /// session has given its own sign of that speech.
    #[must_use]
    pub(crate) fn backed(&self) -> Option<Instant> {
        let armed = self.armed.filter(|armed| armed.backed)?;
        (armed.trigger == BargeTrigger::MicSpeech).then_some(armed.since)
    }

    /// A barge-in is pending: true while only the microphone has armed it.
    #[must_use]
    pub(crate) fn unbacked(&self) -> Option<bool> {
        self.armed.map(|armed| !armed.backed)
    }

    /// A barge-in is armed and has neither fired nor been dropped.
    #[must_use]
    pub(crate) fn pending(&self) -> bool {
        self.armed.is_some()
    }

    /// One reply frame from the model was accepted at `now`.
    pub(crate) fn heard(&mut self, now: Instant, audible: bool) {
        let Some(armed) = &mut self.armed else {
            return;
        };
        if !audible {
            return;
        }
        if now >= armed.until {
            self.armed = None;
        } else {
            armed.quiet_since = now;
        }
    }

    /// When the pending barge-in fires unless the model is heard first:
    /// already at `now` once the model has `ended` its turn.
    #[must_use]
    pub(crate) fn due(&self, now: Instant, ended: bool) -> Option<Instant> {
        let due = self.armed?.quiet_since + BARGE_QUIET;
        Some(if ended { due.min(now) } else { due })
    }

    /// The trigger of a barge-in that is due at `now`; it is no longer pending.
    pub(crate) fn fire(&mut self, now: Instant, ended: bool) -> Option<BargeTrigger> {
        self.due(now, ended).filter(|due| *due <= now)?;
        self.armed.take().map(|armed| armed.trigger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(20);

    /// T1: the model talks through the whole window, so nothing fires.
    #[tokio::test(start_paused = true)]
    async fn barge_is_dropped_when_the_model_talks_past_the_window() {
        let start = Instant::now();
        let mut barge = Barge::default();
        assert_eq!((barge.due(start, true), barge.fire(start, true)), (None, None));
        assert!(!barge.pending() && barge.arm(start, BargeTrigger::UserTranscript));
        assert_eq!(barge.due(start, false), Some(start + BARGE_QUIET));
        let mut now = start;
        while now < start + BARGE_WINDOW {
            now += FRAME;
            assert_eq!(barge.fire(now, false), None);
            barge.heard(now, true);
        }
        assert!(!barge.pending());
        assert_eq!(barge.due(now, false), None);
        assert_eq!(barge.fire(now + BARGE_QUIET, false), None);
    }

    /// T1: quiet frames do not count as the model talking; the quiet stretch
    /// runs from the last audible frame, and a further sign renews the window
    /// without restarting the stretch.
    #[tokio::test(start_paused = true)]
    async fn barge_fires_after_the_quiet_stretch_with_its_first_trigger() {
        let start = Instant::now();
        let mut barge = Barge::default();
        barge.arm(start, BargeTrigger::SpeechStarted);
        barge.heard(start + FRAME * 5, true);
        barge.heard(start + FRAME * 6, false);
        assert!(!barge.arm(start + FRAME * 7, BargeTrigger::UserTranscript));
        assert!(barge.pending());
        let due = start + FRAME * 5 + BARGE_QUIET;
        assert_eq!(barge.due(start, false), Some(due));
        assert_eq!(barge.fire(due - Duration::from_millis(1), false), None);
        assert_eq!(barge.fire(due, false), Some(BargeTrigger::SpeechStarted));
        assert_eq!((barge.due(due, false), barge.fire(due, false)), (None, None));
        assert_eq!(
            (BargeTrigger::UserTranscript.name(), BargeTrigger::SpeechStarted.name()),
            ("user_transcript", "speech_started")
        );
    }

    /// T1: a stretch that began inside the window still fires after it.
    #[tokio::test(start_paused = true)]
    async fn barge_quiet_that_began_inside_the_window_fires_after_it() {
        let start = Instant::now();
        let mut barge = Barge::default();
        barge.arm(start, BargeTrigger::UserTranscript);
        let last = start + BARGE_WINDOW - FRAME;
        barge.heard(last, true);
        barge.heard(start + BARGE_WINDOW, false);
        assert_eq!(
            barge.fire(last + BARGE_QUIET, false),
            Some(BargeTrigger::UserTranscript)
        );
    }

    /// T1: a model that ended its turn has yielded; nothing is waited out.
    #[tokio::test(start_paused = true)]
    async fn barge_fires_at_once_when_the_model_has_ended_its_turn() {
        let start = Instant::now();
        let mut barge = Barge::default();
        barge.arm(start, BargeTrigger::UserTranscript);
        let now = start + FRAME;
        assert_eq!(barge.fire(now, false), None);
        assert_eq!(barge.due(now, true), Some(now));
        assert_eq!(barge.fire(now, true), Some(BargeTrigger::UserTranscript));
    }
}
