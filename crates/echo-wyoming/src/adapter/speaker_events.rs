//! hassmic's `hassmic-speaker` reports as the link's latest speaker state.
//!
//! hassmic sends `{"volume": 0..100, "muted": bool}` once per connection and
//! on every change; `muted` is output mute, never microphone privacy.
use echo_contracts::wyoming::WyomingEvent;
use echo_link::SpeakerState;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::watch;

const EVENT: &str = "hassmic-speaker";

/// The latest speaker state the device reported, shared by a client and its
/// event stream; unknown until a report arrives on the current connection.
#[derive(Clone)]
pub(crate) struct Speaker(Arc<watch::Sender<SpeakerState>>);

impl Default for Speaker {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(SpeakerState::default())))
    }
}

impl Speaker {
    /// Whether `event` is a speaker report. A well-formed one becomes the
    /// latest state; a malformed one is ignored.
    pub(super) fn observe(&self, event: &WyomingEvent) -> bool {
        let WyomingEvent::Other(name, data) = event else {
            return false;
        };
        if name != EVENT {
            return false;
        }
        if let Some(state) = parse(data) {
            self.0
                .send_if_modified(|current| std::mem::replace(current, state) != state);
        }
        true
    }

    /// A new connection reports afresh; until then nothing is known.
    pub(super) fn reset(&self) {
        self.0
            .send_if_modified(|current| std::mem::take(current) != SpeakerState::default());
    }

    /// Resolves with the first state that differs from `previous`.
    pub(super) async fn changed(&self, previous: SpeakerState) -> SpeakerState {
        let mut states = self.0.subscribe();
        // `self` keeps the sender alive, so the wait ends only with a change.
        if let Ok(state) = states.wait_for(|state| *state != previous).await.map(|state| *state) {
            return state;
        }
        std::future::pending().await
    }
}

fn parse(data: &Value) -> Option<SpeakerState> {
    let volume = data.get("volume")?.as_u64().filter(|volume| *volume <= 100)?;
    let muted = data.get("muted")?.as_bool()?;
    Some(SpeakerState {
        volume: Some(u8::try_from(volume).ok()?),
        muted: Some(muted),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(data: Value) -> WyomingEvent {
        WyomingEvent::Other(EVENT.into(), data)
    }

    #[test]
    fn well_formed_reports_set_the_state_and_malformed_ones_are_ignored() {
        let speaker = Speaker::default();
        assert!(speaker.observe(&report(json!({"volume":30,"muted":false}))));
        let reported = SpeakerState {
            volume: Some(30),
            muted: Some(false),
        };
        assert_eq!(*speaker.0.borrow(), reported);
        for data in [
            json!({}),
            json!({"volume":101,"muted":false}),
            json!({"volume":-1,"muted":false}),
            json!({"volume":30.5,"muted":false}),
            json!({"volume":"30","muted":false}),
            json!({"volume":30}),
            json!({"volume":30,"muted":"true"}),
            json!(null),
        ] {
            assert!(speaker.observe(&report(data)), "still a speaker report");
            assert_eq!(*speaker.0.borrow(), reported);
        }
        assert!(!speaker.observe(&WyomingEvent::Other("hassmic-mute".into(), json!({"muted":true}))));
        assert_eq!(*speaker.0.borrow(), reported, "microphone privacy is not output mute");
        speaker.reset();
        assert_eq!(*speaker.0.borrow(), SpeakerState::default());
    }

    #[tokio::test(start_paused = true)]
    async fn changed_waits_for_a_state_other_than_the_previous_one() {
        let speaker = Speaker::default();
        let known = SpeakerState {
            volume: Some(40),
            muted: Some(true),
        };
        speaker.observe(&report(json!({"volume":40,"muted":true})));
        assert_eq!(speaker.changed(SpeakerState::default()).await, known);
        let waiting = speaker.changed(known);
        tokio::pin!(waiting);
        assert!(futures_poll(&mut waiting).is_pending());
        speaker.observe(&report(json!({"volume":40,"muted":true})));
        assert!(
            futures_poll(&mut waiting).is_pending(),
            "an unchanged report wakes nothing"
        );
        speaker.observe(&report(json!({"volume":40,"muted":false})));
        assert_eq!(
            waiting.await,
            SpeakerState {
                volume: Some(40),
                muted: Some(false)
            }
        );
    }

    /// T1: a report before Info passes the startup barrier, and a volume set
    /// is confirmed by the report the device sends back.
    #[tokio::test(start_paused = true)]
    async fn the_link_takes_reports_before_info_and_after_a_set() {
        use crate::{
            ClientOptions, WyomingClient,
            wyoming_test_support::{receive, send},
        };
        use echo_link::{EchoLink, LinkEvent, LinkEvents, SpeakerRequest};
        let (io, mut peer) = tokio::io::duplex(65536);
        let mut client = WyomingClient::from_io(io, ClientOptions::default());
        client.neutral_barrier = true;
        let mut events = EchoLink::events(&mut client).unwrap();
        send(&mut peer, EVENT, json!({"volume":40,"muted":false}), vec![]).await;
        send(&mut peer, "info", json!({}), vec![]).await;
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Other)), "Info");
        let connected = SpeakerState {
            volume: Some(40),
            muted: Some(false),
        };
        assert_eq!(client.speaker_changed(SpeakerState::default()).await, connected);

        let device = async {
            let frame = receive(&mut peer).await;
            assert_eq!(frame.header.r#type, "hassmic-set-volume");
            assert_eq!(frame.header.data.or(frame.data), Some(json!({"volume":30})));
            send(&mut peer, EVENT, json!({"volume":30,"muted":false}), vec![]).await;
        };
        let caller = async {
            client.request_speaker(SpeakerRequest::Volume(30)).await.unwrap();
            client.speaker_changed(connected).await
        };
        let pump = async { while let Ok(Some(_)) = events.recv().await {} };
        let (state, ()) = tokio::select! {
            result = async { tokio::join!(caller, device) } => result,
            () = pump => unreachable!("the stream ended"),
        };
        assert_eq!(
            state,
            SpeakerState {
                volume: Some(30),
                muted: Some(false)
            }
        );
    }

    fn futures_poll<F: std::future::Future>(future: &mut std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
        future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }
}
