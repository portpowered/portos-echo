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
        self.ready_to_close(stopped, mouth_drained) && self.microphone_finished
    }

    /// Playback can settle while duplex capture is still open. Never wait for
    /// the EOF caused by the end command before deciding to issue that command.
    pub fn ready_to_close(&self, stopped: bool, mouth_drained: bool) -> bool {
        !self.cutting() && self.played && stopped && mouth_drained
    }
}

/// Execute the preserving one-reply close after both playback acknowledgments.
/// Keep observing mute and draining accepted capture until explicit-end EOF.
pub(super) async fn close<L: echo_link::EchoLink>(
    client: &L,
    input: (&mut L::Events, &mut <L::Events as echo_link::LinkEvents>::Run),
    port: &impl super::VoicePort,
    source: &mut super::Source,
    ceiling: std::time::Duration,
    cancelled: impl std::future::Future<Output = ()>,
    observe: &mut impl FnMut(super::Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    use echo_link::{LinkEvent, LinkEvents};
    let (events, microphone) = input;
    let work = async {
        if !source.controller.ready_to_close(true, source.mouth.due().is_none()) {
            return Err(Error::Wyoming);
        }
        client.end_run().await?;
        while !source.controller.microphone_finished() {
            tokio::select! {
                event = events.recv() => match event? {
                    Some(event) => super::device(&event, true, source, observe)?,
                    None => return Err(Error::Wyoming),
                },
                frame = super::earcon::microphone(microphone) => {
                    if source.capture(frame?, port)? {
                        observe(super::Notice::Device(LinkEvent::MicrophoneDropped(1)))?;
                    }
                },
            }
        }
        if source.controller.complete(true, source.mouth.due().is_none()) {
            Ok(())
        } else {
            Err(Error::Wyoming)
        }
    };
    tokio::select! {
        biased;
        () = cancelled => Err(Error::Control),
        result = tokio::time::timeout(ceiling, work) => result.map_err(|_| Error::Deadline)?,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MediaEvent, Source, VoicePort};
    use super::*;
    use echo_link::{
        EchoLink, LinkConnector, LinkEvent, LinkEvents,
        fake::{Command, FakeConnector, Observer},
    };

    struct Port;
    impl VoicePort for Port {
        fn send(&self, _: echo_media::PcmFrame) -> Result<(), Error> {
            Ok(())
        }
        async fn next(&mut self) -> Result<MediaEvent, Error> {
            std::future::pending().await
        }
        async fn ready_audio(&mut self) -> Result<Option<echo_media::PcmFrame>, Error> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn session_controller_explicit_end_precedes_capture_eof_and_failure_never_completes() {
        for failure in [None, Some(echo_link::Error::Timeout)] {
            let observer = Observer::default();
            let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
            let mut events = client.events().unwrap();
            observer.wake("Computer", vec![]);
            let Some(LinkEvent::Wake(mut microphone)) = events.recv().await.unwrap() else {
                unreachable!()
            };
            let mut source = Source::new(200, false).unwrap();
            source.controller.reply_started();
            assert_eq!(source.controller.played(true), Ok(true));
            assert!(!source.controller.microphone_finished());
            if let Some(error) = failure {
                observer.end_failure(error);
            }
            let result = close(
                &client,
                (&mut events, &mut microphone),
                &Port,
                &mut source,
                std::time::Duration::from_secs(1),
                std::future::pending(),
                &mut |_| Ok(()),
            )
            .await;
            match failure {
                None => {
                    assert_eq!(result, Ok(()));
                    assert!(source.controller.complete(true, true));
                    assert_eq!(observer.commands(), vec![Command::Connect, Command::EndRun]);
                }
                Some(_) => {
                    assert_eq!(result, Err(Error::Deadline));
                    assert!(!source.controller.complete(true, true));
                    assert_eq!(observer.commands(), vec![Command::Connect]);
                }
            }
        }
    }

    #[test]
    fn session_controller_requires_played_capture_eof_stop_and_mouth_drain() {
        let mut controller = Controller::default();
        controller.reply_started();
        assert!(!controller.complete(true, true));
        assert_eq!(controller.played(false), Err(Error::Wyoming));
        assert!(!controller.has_played());
        assert_eq!(controller.played(true), Ok(true));
        assert!(controller.ready_to_close(true, true));
        assert!(!controller.ready_to_close(false, true));
        assert!(!controller.ready_to_close(true, false));
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
