use crate::{
    barge::{Barge, Floor, Question},
    dispatch::Dispatch,
    run::Run,
};
use echo_core_api::{
    BargeTrigger, SessionCommand, SessionCommands, SessionKnobs, SessionMark as Mark, SessionOutcome, SessionTurnEvent,
};
use std::time::Instant;

#[derive(Debug, Default)]
pub(crate) struct Turn {
    pub(crate) dispatch: Dispatch,
    pub(crate) stale: bool,
    pub(crate) resumed_listen: bool,
    pub(crate) marks: Vec<Mark>,
    barge: Barge,
    question: Question,
    floor: Floor,
    speech: Option<Speech>,
}

#[derive(Debug)]
struct Speech {
    since: Instant,
    heard: Instant,
    transcribed: bool,
}

impl Turn {
    pub(crate) fn resume(&mut self, preserve_dispatch: bool) {
        self.resumed_listen = false;
        if !preserve_dispatch {
            self.dispatch = self.dispatch.carried();
        }
        self.barge = Barge::default();
    }

    pub(crate) fn barge_pending(&self) -> bool {
        self.barge.pending()
    }

    pub(crate) fn barge_due(&self, epoch: Instant, now: Instant, finished: bool) -> Option<Instant> {
        epoch.checked_add(self.barge.due(now.saturating_duration_since(epoch), finished)?)
    }

    pub(crate) fn speaking(&self) -> bool {
        self.speech.is_some()
    }

    pub(crate) fn latch_expiry(&self, knobs: SessionKnobs) -> Option<Instant> {
        self.speech.as_ref()?.heard.checked_add(knobs.expiry)
    }

    fn active(&mut self, now: Instant, transcribed: bool) {
        self.resumed_listen = false;
        let speech = self.speech.get_or_insert(Speech {
            since: now,
            heard: now,
            transcribed,
        });
        speech.heard = now;
        speech.transcribed |= transcribed;
        self.floor.heard();
    }

    fn release(&mut self, now: Instant, reason: &'static str) {
        if let Some(speech) = self.speech.take() {
            self.marks.push(Mark::Latched {
                held: now.saturating_duration_since(speech.since),
                reason,
                transcribed: speech.transcribed,
            });
        }
    }

    fn spend_floor(&mut self, mark: impl FnOnce(u32) -> Mark) {
        let (turn, rearmed) = self.floor.cut();
        self.marks.push(mark(turn));
        if let Some(turn) = rearmed {
            self.marks.push(Mark::BargeRearmed { turn, reason: "ended" });
        }
    }
}

impl Run {
    pub(crate) fn turn_event(
        &mut self,
        event: &SessionTurnEvent,
        idle: &mut Option<Instant>,
        knobs: SessionKnobs,
        now: Instant,
        out: &mut SessionCommands,
    ) -> SessionOutcome {
        use SessionOutcome::{Applied, FinishReply};
        match event {
            SessionTurnEvent::SpeechStarted => {
                return self.heard_user(BargeTrigger::SpeechStarted, "", false, false, knobs, now);
            }
            SessionTurnEvent::SpeechStopped { voiced } => self.user_quiet(now, "cleared", *voiced),
            SessionTurnEvent::Transcript {
                text,
                whole,
                sounded,
                voiced,
            } => {
                let held = self.heard_user(BargeTrigger::UserTranscript, text, *whole, *sounded, knobs, now);
                if *whole {
                    self.user_quiet(now, "cleared", *voiced);
                }
                return held;
            }
            SessionTurnEvent::AssistantStarted => {
                self.reply_started();
                self.turn.dispatch.spoke();
            }
            SessionTurnEvent::AssistantAudio { audible } => return self.heard_assistant(now, *audible),
            SessionTurnEvent::ReplyFrameHanded { voiced } => self.reply_handed(now, *voiced),
            SessionTurnEvent::ReplyDone if self.turn.dispatch.answered() => return FinishReply,
            SessionTurnEvent::DispatchStarted => self.turn.dispatch.started(),
            SessionTurnEvent::DispatchCompleted { answered, listening } => {
                return self.complete_dispatch(idle, now, *answered, *listening);
            }
            SessionTurnEvent::DispatchWhileListening if !self.turn.dispatch.in_flight() => {
                self.turn.dispatch = Dispatch::default();
            }
            SessionTurnEvent::EndDispatch { farewell } => {
                let held = self.turn.dispatch.held();
                self.turn.dispatch.ended(*farewell);
                if held && *farewell {
                    return FinishReply;
                }
            }
            SessionTurnEvent::BargeDue { finished } => self.interrupt(now, *finished, out),
            SessionTurnEvent::ReplyMeasured { cut: false } => self.turn.question = Question::default(),
            SessionTurnEvent::RunEnded => self.turn.release(now, "run_end"),
            SessionTurnEvent::ReplyDone
            | SessionTurnEvent::DispatchWhileListening
            | SessionTurnEvent::ReplyMeasured { cut: true } => (),
        }
        Applied
    }

    fn complete_dispatch(
        &mut self,
        idle: &mut Option<Instant>,
        now: Instant,
        answered: bool,
        listening: bool,
    ) -> SessionOutcome {
        let waiting = self.turn.dispatch.in_flight();
        self.turn.dispatch.completed(now);
        if self.turn.dispatch.in_flight() {
            return SessionOutcome::Applied;
        }
        if listening {
            if waiting {
                self.renew_idle(idle, now);
            }
        } else if !answered && self.ending() && self.turn.dispatch.released() {
            return SessionOutcome::FinishReply;
        }
        SessionOutcome::Applied
    }

    fn heard_user(
        &mut self,
        trigger: BargeTrigger,
        text: &str,
        whole: bool,
        sounded: bool,
        knobs: SessionKnobs,
        now: Instant,
    ) -> SessionOutcome {
        if !whole {
            self.turn.active(now, matches!(trigger, BargeTrigger::UserTranscript));
        }
        let replying = !self.turn.dispatch.in_flight() && self.speaking();
        let over = match trigger {
            BargeTrigger::UserTranscript => self.turn.question.transcribed(whole, replying && sounded),
            BargeTrigger::SpeechStarted => self.turn.question.speech_started(),
        };
        let held = over && replying && self.talked_over(trigger, knobs, now);
        self.turn.barge.transcribed(text, whole);
        if held {
            SessionOutcome::Held
        } else {
            SessionOutcome::Applied
        }
    }

    fn talked_over(&mut self, trigger: BargeTrigger, knobs: SessionKnobs, now: Instant) -> bool {
        let turn = &mut self.turn;
        let since_epoch = now.saturating_duration_since(self.epoch);
        if turn.barge.pending() {
            turn.barge.arm(since_epoch, trigger);
            return false;
        }
        match turn.floor.claim() {
            Err((turn_id, count)) => turn.marks.push(Mark::BargePassed {
                turn: turn_id,
                trigger,
                count,
            }),
            Ok(_) if !knobs.local_cut => {
                turn.question.cut();
                turn.spend_floor(|turn| Mark::BargeLeft { turn, trigger });
            }
            Ok(_) => return turn.barge.arm(since_epoch, trigger),
        }
        false
    }

    fn heard_assistant(&mut self, now: Instant, audible: bool) -> SessionOutcome {
        let barge = &mut self.turn.barge;
        let held = barge.pending();
        barge.heard(now.saturating_duration_since(self.epoch), audible);
        if held && !barge.pending() {
            SessionOutcome::Released
        } else {
            SessionOutcome::Applied
        }
    }

    fn reply_handed(&mut self, now: Instant, voiced: bool) {
        if voiced && let Ok(due) = self.replacement_deadline(now) {
            self.response_due = self.response_due.max(due);
        }
    }

    fn interrupt(&mut self, now: Instant, finished: bool, out: &mut SessionCommands) {
        let Some(trigger) = self
            .turn
            .barge
            .fire(now.saturating_duration_since(self.epoch), finished)
        else {
            return;
        };
        if self.turn.dispatch.in_flight() || !self.speaking() {
            return;
        }
        out.cut_text = self.turn.barge.said();
        self.turn.spend_floor(|turn| Mark::BargeCut { turn, trigger });
        self.turn.question.cut();
        self.turn.dispatch = Dispatch::default();
        self.cut();
        out.effect = Some(SessionCommand::CutReply {
            session: self.handle,
            trigger,
        });
    }

    pub(crate) fn user_quiet(&mut self, now: Instant, reason: &'static str, voiced: bool) {
        if let Ok(due) = self.replacement_deadline(now) {
            let resumed = (reason == "expired")
                .then(|| self.resumed_listen(now, voiced, due))
                .flatten();
            self.turn.resumed_listen = resumed.is_some();
            self.response_due = resumed.unwrap_or(due);
        }
        self.turn.release(now, reason);
        if let Some(turn) = self.turn.floor.ended(self.turn.barge.pending()) {
            self.turn.marks.push(Mark::BargeRearmed { turn, reason });
        }
    }

    fn resumed_listen(&self, now: Instant, voiced: bool, due: Instant) -> Option<Instant> {
        if self.follow_up != echo_core_api::FollowUp::Replied
            || self.turn.dispatch.waiting()
            || self.speaking()
            || voiced
        {
            return None;
        }
        Some(self.projected_listen(now)?.min(due))
    }
}
