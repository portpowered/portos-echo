//! Schedule World-selected waits at the existing biased voice-owner fences.
use super::{Error, Source};
use echo_core_api::{SessionOutcome, SessionWait};
use tokio::time::Instant;
#[path = "run_marks.rs"]
pub(super) mod run_marks;
pub(super) use echo_core_api::SessionMark as Mark;
#[cfg(test)]
pub(super) use echo_sub_session::HANDBACK_GRACE;

pub(super) fn deadline(source: &Source, played: Option<Instant>) -> (Option<Instant>, SessionWait) {
    source
        .controller
        .reply_wait(played, source.reply.finished(), source.reply.voiced())
}

pub(super) fn listening(source: &Source, idle: Instant) -> (Option<Instant>, SessionWait) {
    source.controller.listening_wait(idle)
}

pub(super) fn listen_lapsed(
    source: &mut Source,
    due: Option<Instant>,
    wait: SessionWait,
) -> Option<Result<bool, Error>> {
    match source.controller.wait_expired(wait, due, source.reply.voiced(), true) {
        Ok(SessionOutcome::Applied) => None,
        Ok(SessionOutcome::WaitExpired) if wait == SessionWait::Listen => Some(Ok(false)),
        Ok(SessionOutcome::WaitExpired) => Some(Err(error(wait))),
        Err(error) => Some(Err(error)),
        _ => Some(Err(Error::Control)),
    }
}

pub(super) fn expired(source: &mut Source, due: Option<Instant>, wait: SessionWait) -> Result<(), Error> {
    match source
        .controller
        .wait_expired(wait, due, source.reply.voiced(), false)?
    {
        SessionOutcome::Applied => Ok(()),
        SessionOutcome::Unanswered => source.reply.finish(),
        SessionOutcome::WaitExpired => Err(error(wait)),
        _ => Err(Error::Control),
    }
}

fn error(wait: SessionWait) -> Error {
    match wait {
        SessionWait::Playback | SessionWait::Listen => Error::Deadline,
        SessionWait::Response => Error::NoResponse,
        SessionWait::Ceiling => Error::SessionCeiling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_core_api::SessionTurnEvent;
    use std::time::Duration;

    fn started(source: &Source) {
        source.controller.turn(SessionTurnEvent::DispatchStarted);
    }

    fn answered(source: &Source) -> bool {
        source.controller.turn(SessionTurnEvent::ReplyDone).outcome == Some(Ok(SessionOutcome::FinishReply))
    }

    fn reply(source: &Source, played: Option<Instant>) -> (Option<Instant>, Error) {
        let (due, wait) = deadline(source, played);
        (due, error(wait))
    }

    fn listen(source: &Source, idle: Instant) -> (Option<Instant>, Result<bool, Error>) {
        let (due, wait) = listening(source, idle);
        (
            due,
            if wait == SessionWait::Listen {
                Ok(false)
            } else {
                Err(error(wait))
            },
        )
    }

    #[test]
    fn delegated_completion_does_not_reopen_a_finished_reply() {
        let mut source = Source::new(200, false).unwrap();
        started(&source);
        source.spoke();
        assert!(!answered(&source)); // Filler.
        source.spoke();
        assert!(answered(&source));
        source.dispatch_completed();
        assert!(!source.controller.dispatch_waiting());
    }

    #[tokio::test(start_paused = true)]
    async fn delegated_wait_keeps_its_admission_ceiling_through_speech_and_playback() {
        let opened = Instant::now();
        let maximum = Duration::from_secs(90);
        let response = opened + Duration::from_secs(60);
        let mut source = Source::with_session(200, false, opened, Some(maximum)).unwrap();
        let ceiling = opened + maximum;
        source.controller.set_response_due(response);
        assert_eq!(reply(&source, None), (Some(response), Error::NoResponse));
        started(&source);
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(reply(&source, None), (Some(ceiling), Error::SessionCeiling));
        source.dispatch_completed();
        started(&source);
        tokio::time::advance(Duration::from_secs(30)).await;
        source.speech_started();
        // Speech defers the answer's wait, and the ceiling by one expiry at most.
        let deferred = ceiling + run_marks::SPEECH_START_EXPIRY;
        assert_eq!(reply(&source, None), (Some(deferred), Error::SessionCeiling));
        source.user_quiet();
        source.spoke();
        assert_eq!(reply(&source, None).0, None);
        assert!(!answered(&source));
        let audible = [2000_i16.to_le_bytes(); 480].concat();
        source.reply.push_pcm(audible.as_slice().try_into().unwrap());
        assert!(source.reply.stage(false).await.is_some());
        source.reply.handed();
        assert_eq!(reply(&source, None).0, None);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(source.reply.mouth_change(), Some(true));
        assert_eq!(source.reply.mouth_change(), Some(false));
        assert_eq!(reply(&source, None), (Some(ceiling), Error::SessionCeiling));
        source.dispatch_completed();
        source.spoke();
        assert!(answered(&source));
        source.reply.finish().unwrap();
        assert_eq!(reply(&source, None), (None, Error::Deadline));
        let played = Instant::now() + Duration::from_secs(5);
        assert_eq!(reply(&source, Some(played)), (Some(played), Error::Deadline));
    }

    /// A run admitted at `opened` with the Echo's own windows: a 15 s listen
    /// window and the default `voice.session_max_seconds` of 600 s.
    fn admitted(opened: Instant) -> (Source, Instant) {
        let maximum = Duration::from_secs(600);
        let source = Source::with_session(200, false, opened, Some(maximum)).unwrap();
        let ceiling = opened + maximum;
        source.controller.set_response_due(opened + Duration::from_secs(15));
        (source, ceiling)
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_delegated_turn_outlives_the_no_answer_and_listen_windows() {
        let (source, ceiling) = admitted(Instant::now());
        let idle = Instant::now() + Duration::from_secs(15);
        started(&source);
        // Two minutes of thinking and tool calls, far past both windows.
        tokio::time::advance(Duration::from_secs(120)).await;
        assert_eq!(reply(&source, None), (Some(ceiling), Error::SessionCeiling));
        assert_eq!(listen(&source, idle), (Some(ceiling), Err(Error::SessionCeiling)));
        // Its completion re-arms the wait: the answer's grace, then no answer.
        source.dispatch_completed();
        let owed = Instant::now() + HANDBACK_GRACE;
        assert_eq!(reply(&source, None), (Some(owed), Error::NoResponse));
        assert_eq!(listen(&source, idle), (Some(owed), Err(Error::NoResponse)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_session_with_nothing_in_progress_still_ends_at_its_windows() {
        let (source, _) = admitted(Instant::now());
        let idle = Instant::now() + Duration::from_secs(15);
        assert!(!source.controller.dispatch_waiting());
        assert_eq!(reply(&source, None), (Some(idle), Error::NoResponse));
        assert_eq!(listen(&source, idle), (Some(idle), Ok(false)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_sentence_across_the_ceiling_is_heard_to_its_end() {
        let (mut source, ceiling) = admitted(Instant::now());
        started(&source);
        tokio::time::advance(Duration::from_secs(599)).await;
        source.user_transcript("speech".into(), false);
        // Its text renews the latch past the ceiling; the cap waits one expiry.
        tokio::time::advance(Duration::from_secs(6)).await;
        source.user_transcript("speech".into(), false);
        let idle = Instant::now() + Duration::from_secs(15);
        let deferred = ceiling + run_marks::SPEECH_START_EXPIRY;
        assert_eq!(reply(&source, None), (Some(deferred), Error::SessionCeiling));
        assert_eq!(listen(&source, idle), (Some(deferred), Err(Error::SessionCeiling)));
        source.user_quiet();
        assert_eq!(reply(&source, None), (Some(ceiling), Error::SessionCeiling));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_delegated_turn_ends_at_the_run_ceiling() {
        let opened = Instant::now();
        let (source, ceiling) = admitted(opened);
        started(&source);
        tokio::time::advance(Duration::from_secs(599)).await;
        let idle = Instant::now() + Duration::from_secs(15);
        assert_eq!(reply(&source, None), (Some(ceiling), Error::SessionCeiling));
        assert_eq!(listen(&source, idle), (Some(ceiling), Err(Error::SessionCeiling)));
        assert_eq!(ceiling.duration_since(opened), Duration::from_secs(600));
    }
}
