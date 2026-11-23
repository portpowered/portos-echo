//! Run lifecycle I/O at the playback and capture acknowledgement fences.
use super::Error;
pub(super) use crate::bridge_world::SessionPort as Controller;

/// Execute controller-selected close after both playback acknowledgments.
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
        source.controller.begin_close(source.reply.mouth_due().is_none())?;
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
        source.controller.finish_close(source.reply.mouth_due().is_none())?;
        if source.controller.complete(true, source.reply.mouth_due().is_none()) {
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
    use tokio::time::{Duration, Instant};

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

    #[test]
    fn session_controller_closing_rejects_activity_and_requires_successful_end() {
        let mut source = Source::new(200, false).unwrap();
        assert_eq!(source.controller.begin_close(true), Err(Error::Wyoming));
        source.controller.reply_started();
        source.controller.played(true).unwrap();
        source.controller.listen_after_reply(Duration::from_secs(3)).unwrap();
        assert_eq!(source.controller.begin_close(false), Err(Error::Wyoming));
        source.controller.begin_close(true).unwrap();
        assert_eq!(source.controller.idle_deadline(), None);
        assert_close_rejects_activity(&mut source);
        assert_eq!(source.controller.finish_close(true), Err(Error::Wyoming));
        source.controller.microphone_ended();
        assert!(!source.controller.complete(true, true));
        assert_eq!(source.controller.finish_close(false), Err(Error::Wyoming));
        source.controller.finish_close(true).unwrap();
        assert!(source.controller.complete(true, true));
        assert_close_rejects_activity(&mut source);
        assert_eq!(source.controller.finish_close(true), Err(Error::Wyoming));
    }

    fn assert_close_rejects_activity(source: &mut Source) {
        assert!(!source.controller.accepts_audio());
        assert!(!source.controller.speaking());
        assert_eq!(source.resume_reply(false), Err(Error::Control));
        assert_eq!(
            source.controller.listen_after_reply(Duration::from_secs(3)),
            Err(Error::Input)
        );
        assert_eq!(source.controller.begin_close(true), Err(Error::Wyoming));
    }

    #[tokio::test(start_paused = true)]
    async fn session_controller_cancelled_close_never_writes_end_or_reports_completion() {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.wake("Computer", vec![]);
        let Some(LinkEvent::Wake(mut microphone)) = events.recv().await.unwrap() else {
            unreachable!()
        };
        let mut source = Source::new(200, false).unwrap();
        source.controller.reply_started();
        source.controller.played(true).unwrap();
        let result = close(
            &client,
            (&mut events, &mut microphone),
            &Port,
            &mut source,
            std::time::Duration::from_secs(1),
            std::future::ready(()),
            &mut |_| Ok(()),
        )
        .await;
        assert_eq!(result, Err(Error::Control));
        assert_eq!(observer.commands(), [Command::Connect]);
        assert!(!source.controller.complete(true, true));
        assert!(source.controller.ready_to_close(true, true));
    }

    #[tokio::test(start_paused = true)]
    async fn session_controller_listening_requires_settlement_and_renews_only_on_activity() {
        let controller = Controller::open(Instant::now(), None).unwrap();
        let interval = Duration::from_secs(3);
        assert_eq!(controller.listen_after_reply(interval), Err(Error::Input));
        controller
            .admit_response(Instant::now() + Duration::from_secs(10))
            .unwrap();
        controller.reply_started();
        controller.played(true).unwrap();
        assert_eq!(controller.listen_after_reply(Duration::ZERO), Err(Error::Input));
        let first = Instant::now();
        controller.listen_after_reply(interval).unwrap();
        assert_eq!(controller.idle_deadline(), Some(first + interval));
        assert!(!controller.accepts_audio());
        assert!(controller.ready_to_close(true, true));
        tokio::time::advance(Duration::from_secs(2)).await;
        controller.resume(false).unwrap();
        assert_eq!(controller.idle_deadline(), None);
        assert!(controller.accepts_audio());
        assert!(!controller.has_played());
        assert!(!controller.ready_to_close(true, true));
        assert_eq!(
            controller.replacement_deadline(),
            Ok(Instant::now() + Duration::from_secs(10))
        );
        controller.reply_started();
        controller.played(true).unwrap();
        controller.listen_after_reply(interval).unwrap();
        assert_eq!(controller.idle_deadline(), Some(first + Duration::from_secs(5)));
    }

    #[tokio::test(start_paused = true)]
    async fn session_controller_replacement_budget_is_bounded_and_requires_admission() {
        let controller = Controller::open(Instant::now(), None).unwrap();
        let start = Instant::now();
        assert_eq!(controller.replacement_deadline(), Err(Error::Input));
        for deadline in [start - Duration::from_secs(1), start] {
            assert_eq!(controller.admit_response(deadline), Err(Error::Deadline));
            assert_eq!(controller.replacement_deadline(), Err(Error::Input));
        }
        controller.admit_response(start + Duration::from_secs(10)).unwrap();
        tokio::time::advance(Duration::from_secs(9)).await;
        controller.reply_started();
        controller.cut();
        controller.cut_settled();
        let deadline = controller.replacement_deadline().unwrap();
        assert_eq!(deadline, start + Duration::from_secs(19));
        tokio::time::sleep_until(deadline).await;
        assert_eq!(start.elapsed(), Duration::from_secs(19));
    }

    #[tokio::test(start_paused = true)]
    async fn session_controller_explicit_end_precedes_capture_eof_and_failure_never_completes() {
        for (failure, input_ended) in [
            (None, false),
            (None, true),
            (Some(echo_link::Error::Timeout), false),
            (Some(echo_link::Error::Timeout), true),
        ] {
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
            if input_ended {
                source.controller.microphone_ended();
            }
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
            if failure.is_none() {
                assert_eq!(result, Ok(()));
                assert!(source.controller.complete(true, true));
                assert_eq!(observer.commands(), vec![Command::Connect, Command::EndRun]);
            } else {
                assert_eq!(result, Err(Error::Deadline));
                assert!(!source.controller.complete(true, true));
                assert!(!source.controller.accepts_audio());
                assert_eq!(source.resume_reply(false), Err(Error::Control));
                assert_eq!(source.controller.begin_close(true), Err(Error::Wyoming));
                assert_eq!(observer.commands(), vec![Command::Connect]);
            }
        }
    }

    #[test]
    fn session_controller_requires_played_capture_eof_stop_and_mouth_drain() {
        let controller = Controller::open(Instant::now(), None).unwrap();
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
        controller.begin_close(true).unwrap();
        controller.finish_close(true).unwrap();
        assert!(!controller.complete(false, true));
        assert!(!controller.complete(true, false));
        assert!(controller.complete(true, true));
    }

    #[test]
    fn session_controller_late_cut_receipt_cannot_complete_replacement() {
        let controller = Controller::open(Instant::now(), None).unwrap();
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
        controller.begin_close(true).unwrap();
        controller.finish_close(true).unwrap();
        assert!(controller.complete(true, true));
    }

    #[test]
    fn session_controller_cut_after_played_does_not_steal_replacement_receipt() {
        let controller = Controller::open(Instant::now(), None).unwrap();
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
    fn session_controller_speaks_only_between_reply_start_and_cut() {
        let controller = Controller::open(Instant::now(), None).unwrap();
        assert!(controller.accepts_audio());
        assert!(!controller.speaking());
        controller.reply_started();
        assert!(controller.speaking());
        controller.cut();
        assert!(!controller.speaking());
        controller.cut_settled();
        assert!(!controller.speaking());
        controller.reply_started();
        assert!(controller.speaking());
    }
}
