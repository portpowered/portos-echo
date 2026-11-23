//! A barge-in silences the reply at once and cuts it once the model yields.
use echo_core_api::BargeTrigger;
use std::time::Duration;

/// How long a sign of user speech from the Live session keeps a barge-in
/// pending. Each further sign renews it.
pub const BARGE_WINDOW: Duration = Duration::from_secs(2);
/// A pending barge-in fires once the reply has gone this long without
/// audible audio from the model: it stopped talking, so the user has the floor.
pub const BARGE_QUIET: Duration = Duration::from_millis(500);
/// The most of the user's transcript kept for a cut's trace row, in bytes.
const SAID_MAX: usize = 240;

#[derive(Clone, Copy, Debug)]
struct Armed {
    trigger: BargeTrigger,
    until: Duration,
    quiet_since: Duration,
}

/// The Live session is the only judge of a barge-in: the bridge runs no
/// speech detector of its own and never gates the microphone it forwards.
/// The session transcribes whatever that microphone hears while a reply
/// plays: the user, other voices in the room, the Echo's own speaker. The
/// model decides for itself whether to yield, and the pinned session has no
/// request that makes it and no event that says it did. A sign of user
/// speech from the session, the earliest being the first fragment of the
/// user's transcript, therefore arms a barge-in.
/// While it is pending the reply is held: nothing more is written to the
/// device, so the user hears it stop, and the model's audio waits in the
/// reply queue. It fires when the model's reply audio then stays quiet for
/// `BARGE_QUIET`, counted from the sign or from the last audible frame after
/// it: the reply is cut and the held audio dropped. A model still talking
/// when `BARGE_WINDOW` has passed was not interrupted: the barge-in is
/// dropped and the held audio plays, late but whole.
/// A model that has ended its turn has said so itself: there is no stretch to
/// wait out, and a pending barge-in fires at once.
#[derive(Debug, Default)]
pub(crate) struct Barge {
    armed: Option<Armed>,
    /// The user's transcript since the pending barge-in was armed.
    said: String,
}

impl Barge {
    /// A sign of user speech came at `now` during a reply. True when it
    /// armed a barge-in, false when it renewed the pending one.
    pub(crate) fn arm(&mut self, now: Duration, trigger: BargeTrigger) -> bool {
        let until = now + BARGE_WINDOW;
        if let Some(armed) = &mut self.armed {
            armed.until = armed.until.max(until);
            return false;
        }
        self.said.clear();
        self.armed = Some(Armed {
            trigger,
            until,
            quiet_since: now,
        });
        true
    }

    /// The session's transcript of the user while a barge-in is pending: a
    /// fragment, or the `whole` turn. Kept to `SAID_MAX` for the trace.
    pub(crate) fn transcribed(&mut self, text: &str, whole: bool) {
        if self.armed.is_none() {
            return;
        }
        if whole {
            self.said.clear();
        }
        for letter in text.chars() {
            if self.said.len() + letter.len_utf8() > SAID_MAX {
                break;
            }
            self.said.push(letter);
        }
    }

    /// What the session transcribed under the barge-in that just fired.
    pub(crate) fn said(&mut self) -> String {
        std::mem::take(&mut self.said)
    }

    /// A barge-in is armed and has neither fired nor been dropped.
    #[must_use]
    pub(crate) fn pending(&self) -> bool {
        self.armed.is_some()
    }

    /// One reply frame from the model was accepted at `now`.
    pub(crate) fn heard(&mut self, now: Duration, audible: bool) {
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
    pub(crate) fn due(&self, now: Duration, ended: bool) -> Option<Duration> {
        let due = self.armed?.quiet_since + BARGE_QUIET;
        Some(if ended { due.min(now) } else { due })
    }

    /// The trigger of a barge-in that is due at `now`; it is no longer pending.
    pub(crate) fn fire(&mut self, now: Duration, ended: bool) -> Option<BargeTrigger> {
        self.due(now, ended).filter(|due| *due <= now)?;
        self.armed.take().map(|armed| armed.trigger)
    }
}

/// The session's transcript of the turn a reply answers. It has no identity
/// and no fixed place: on the Echo it came as the answer began, some or all
/// of it after the answer's first audio, and held that answer for the whole
/// of `BARGE_WINDOW`. Each reply therefore owns the first user turn
/// transcribed since the reply before it, through that turn's whole
/// transcript. Only a later turn is the user talking over the reply.
#[derive(Debug, Default)]
pub(crate) struct Question {
    asked: Asked,
    /// The user's turn last heard of has been transcribed whole.
    ended: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Asked {
    /// Nothing of the question has come: the next user turn is it.
    #[default]
    No,
    /// Fragments of it have come; the rest is still the question.
    Partly,
    Whole,
}

impl Question {
    /// The session transcribed a fragment of a user turn, or the `whole` of
    /// it; `sounded` says a reply is playing. True when this is the user
    /// talking over that reply, false when it is of a reply's question.
    pub(crate) fn transcribed(&mut self, whole: bool, sounded: bool) -> bool {
        self.ended = whole;
        if sounded && self.asked == Asked::Whole {
            return true;
        }
        self.asked = if whole { Asked::Whole } else { Asked::Partly };
        false
    }

    /// The session's speech-start item: a user turn began, and its
    /// transcript is still to come. Always a sign of the user talking.
    pub(crate) fn speech_started(&mut self) -> bool {
        self.ended = false;
        true
    }

    /// The reply was cut. The turn that cut it is the next reply's question.
    pub(crate) fn cut(&mut self) {
        self.asked = if self.ended { Asked::Whole } else { Asked::Partly };
    }
}

/// One cut per user turn (owner, 2026-11-21). The first sign of the user
/// talking over a reply may arm a barge-in; once it has cut, the rest of that
/// user turn cuts nothing more, so the model's backchannels ("mm-hm") under
/// it play through. The turn ends where the Live session says it does: the
/// user's whole transcript, its end of speech, the end of capture, or the
/// speech-start expiry. Then the cut re-arms for the next turn.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Floor {
    /// Counts the user turns that have cut, from 1: the turn now open.
    turn: u32,
    /// This turn has had its cut.
    spent: bool,
    /// The user has been heard since the turn last ended.
    open: bool,
    /// Signs passed in this turn since its cut.
    passed: u32,
}

impl Default for Floor {
    fn default() -> Self {
        Self {
            turn: 1,
            spent: false,
            open: false,
            passed: 0,
        }
    }
}

impl Floor {
    /// The user is speaking: their turn is open.
    pub(crate) fn heard(&mut self) {
        self.open = true;
    }

    /// A sign of the user talking over a reply, with no barge-in pending:
    /// the turn that may cut, or the turn that already has and how many
    /// signs it has passed since.
    pub(crate) fn claim(&mut self) -> Result<u32, (u32, u32)> {
        if self.spent {
            self.passed += 1;
            return Err((self.turn, self.passed));
        }
        Ok(self.turn)
    }

    /// The turn had its cut: the turn, and the same turn again when it had
    /// already ended, since the cut then re-arms at once.
    pub(crate) fn cut(&mut self) -> (u32, Option<u32>) {
        let turn = self.turn;
        self.spent = true;
        (turn, (!self.open).then(|| self.rearm()))
    }

    /// The user's turn ended. The turn that re-armed, when it had cut and
    /// no barge-in is still `pending`: one pending cuts and then re-arms.
    pub(crate) fn ended(&mut self, pending: bool) -> Option<u32> {
        self.open = false;
        (self.spent && !pending).then(|| self.rearm())
    }

    fn rearm(&mut self) -> u32 {
        let turn = self.turn;
        self.turn += 1;
        self.spent = false;
        self.passed = 0;
        turn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(20);

    /// T1: the transcript under a pending barge-in is kept for its cut,
    /// bounded, and a whole turn replaces its fragments.
    #[test]
    fn barge_keeps_the_transcript_that_armed_it() {
        let start = Duration::ZERO;
        let mut barge = Barge::default();
        barge.transcribed("before", false);
        assert!(barge.arm(start, BargeTrigger::UserTranscript));
        barge.transcribed("Sto", false);
        barge.transcribed("p it", false);
        assert_eq!(barge.fire(start, true), Some(BargeTrigger::UserTranscript));
        assert_eq!((barge.said().as_str(), barge.said().as_str()), ("Stop it", ""));
        barge.arm(start, BargeTrigger::UserTranscript);
        barge.transcribed("Sto", false);
        barge.transcribed("Stop it.", true);
        assert_eq!(barge.said(), "Stop it.");
        barge.arm(start, BargeTrigger::UserTranscript);
        barge.transcribed(&"é".repeat(SAID_MAX), false);
        assert_eq!(barge.said().len(), SAID_MAX);
    }

    /// T1: the model talks through the whole window, so nothing fires.
    #[test]
    fn barge_is_dropped_when_the_model_talks_past_the_window() {
        let start = Duration::ZERO;
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
    #[test]
    fn barge_fires_after_the_quiet_stretch_with_its_first_trigger() {
        let start = Duration::ZERO;
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
    #[test]
    fn barge_quiet_that_began_inside_the_window_fires_after_it() {
        let start = Duration::ZERO;
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
    #[test]
    fn barge_fires_at_once_when_the_model_has_ended_its_turn() {
        let start = Duration::ZERO;
        let mut barge = Barge::default();
        barge.arm(start, BargeTrigger::UserTranscript);
        let now = start + FRAME;
        assert_eq!(barge.fire(now, false), None);
        assert_eq!(barge.due(now, true), Some(now));
        assert_eq!(barge.fire(now, true), Some(BargeTrigger::UserTranscript));
    }

    /// T1: a reply owns the first user turn transcribed since the reply
    /// before it, however late, and a turn that cut a reply is the next
    /// reply's question.
    #[test]
    fn question_is_the_first_turn_since_the_reply_before_and_never_a_barge() {
        // All of it trails the reply's first audio.
        let mut question = Question::default();
        assert!(!question.transcribed(false, true) && !question.transcribed(true, true));
        // The next turn talks over the reply and cuts it before it is whole.
        assert!(question.transcribed(false, true));
        question.cut();
        assert!(!question.transcribed(true, true) && question.transcribed(false, true));
        // Cut after the turn was whole: nothing more of the question comes.
        assert!(question.transcribed(true, true));
        question.cut();
        assert!(question.transcribed(false, true));
        // A turn transcribed with no reply playing is the next question.
        assert!(!question.transcribed(false, false) && !question.transcribed(false, true));
        assert!(!question.transcribed(true, true) && question.transcribed(false, true));
        // A speech-start is always the user, and its turn is not whole.
        assert!(question.speech_started());
        question.cut();
        assert!(!question.transcribed(false, true));
    }

    /// T1: one cut per user turn; the rest of the turn passes and counts what
    /// it passed; the turn's end re-arms, also when it ended before its cut.
    #[test]
    fn floor_cuts_once_per_user_turn_and_rearms_when_it_ends() {
        let mut floor = Floor::default();
        floor.heard();
        assert_eq!(floor.claim(), Ok(1));
        assert_eq!(floor.cut(), (1, None));
        assert_eq!((floor.claim(), floor.claim()), (Err((1, 1)), Err((1, 2))));
        assert_eq!(floor.ended(false), Some(1));
        assert_eq!(floor.ended(false), None);
        floor.heard();
        assert_eq!(floor.claim(), Ok(2));
        // The turn ends under its pending barge-in: it re-arms at the cut.
        assert_eq!(floor.ended(true), None);
        assert_eq!(floor.cut(), (2, Some(2)));
        assert_eq!(floor.claim(), Ok(3));
    }
}
