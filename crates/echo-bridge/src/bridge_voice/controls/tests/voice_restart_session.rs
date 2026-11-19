//! T1: a settled call stops reuse without admitting a queued wake or media.
use super::*;
use crate::bridge_settings::settings_restart::Restart;
use echo_link::{
    LinkConnector, MicrophoneRun,
    fake::{FakeConnector, Observer},
};

struct Port;
impl VoicePort for Port {
    fn send(&self, _: echo_media::PcmFrame) -> Result<(), Error> {
        unreachable!("idle restart sent audio")
    }
    fn next(&mut self) -> impl Future<Output = Result<MediaEvent, Error>> {
        std::future::poll_fn(|_| unreachable!("idle restart consumed control"))
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<echo_media::PcmFrame>, Error>> {
        std::future::poll_fn(|_| unreachable!("idle restart admitted another wake"))
    }
}

#[tokio::test(start_paused = true)]
async fn settings_restart_settled_call_stops_reuse_and_shutdown_wins() {
    for cancelled in [false, true] {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        observer.wake("queued wake", vec![]);
        let restart = Restart::default();
        restart.request();
        let now = Instant::now();
        let result = restart
            .scope(wait(
                &mut events,
                &mut Port,
                Some(now + Duration::from_secs(300)),
                &mut false,
                async {
                    if !cancelled {
                        std::future::pending::<()>().await;
                    }
                },
                &mut |_| unreachable!("idle restart projected activity"),
            ))
            .await;
        if cancelled {
            assert!(matches!(result, Err(Error::Control)));
        } else {
            assert!(matches!(result, Ok(None)));
        }
        assert_eq!(Instant::now(), now);
        let Some(LinkEvent::Wake(run)) = events.recv().await.unwrap() else {
            unreachable!()
        };
        assert_eq!(run.wake_phrase(), "queued wake");
        assert_eq!(observer.commands(), [echo_link::fake::Command::Connect]);
    }
}
