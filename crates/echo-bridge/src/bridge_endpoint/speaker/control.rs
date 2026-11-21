use super::{
    commands,
    voice::{View, Voice},
};
use echo_contracts::endpoint::{AdjustVolume, SetOutputMute, SetVolume};
use echo_link::{EchoLink, SpeakerRequest, SpeakerState};
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{Instant, timeout_at},
};

#[derive(Clone, Default)]
pub(super) struct Control(watch::Sender<Option<mpsc::Sender<Request>>>);

enum Change {
    Absolute(SpeakerRequest),
    Relative(i64),
}

struct Request {
    change: Change,
    sent: oneshot::Sender<()>,
}

struct Attachment<'a>(&'a Control, &'a watch::Sender<View>);

impl Drop for Attachment<'_> {
    fn drop(&mut self) {
        self.0.0.send_replace(None);
        self.1
            .send_if_modified(|view| std::mem::take(&mut view.speaker) != SpeakerState::default());
    }
}

impl Control {
    pub(super) async fn scope<T>(
        &self,
        client: &impl EchoLink,
        view: &watch::Sender<View>,
        work: impl Future<Output = T>,
    ) -> T {
        let (send, receive) = mpsc::channel(8);
        self.0.send_replace(Some(send));
        let _attachment = Attachment(self, view);
        tokio::select! {
            result = work => result,
            () = reports(client, view) => std::future::pending().await,
            () = send_requests(client, view, receive) => std::future::pending().await,
        }
    }

    async fn send(&self, change: Change, deadline: Instant) {
        let (sent, wait) = oneshot::channel();
        let Some(sender) = self.0.borrow().clone() else { return };
        if Instant::now() < deadline && sender.try_send(Request { change, sent }).is_ok() {
            let _ = timeout_at(deadline, wait).await;
        }
    }
}

async fn reports(client: &impl EchoLink, view: &watch::Sender<View>) {
    let mut previous = SpeakerState::default();
    loop {
        previous = client.speaker_changed(previous).await;
        let speaker = SpeakerState {
            volume: previous.volume.filter(|volume| *volume <= 100),
            ..previous
        };
        view.send_if_modified(|view| std::mem::replace(&mut view.speaker, speaker) != speaker);
    }
}

async fn send_requests(client: &impl EchoLink, view: &watch::Sender<View>, mut requests: mpsc::Receiver<Request>) {
    while let Some(Request { change, mut sent }) = requests.recv().await {
        let request = match change {
            Change::Absolute(request) => Some(request),
            Change::Relative(delta) => view.borrow().speaker.volume.and_then(|base| {
                let volume = i64::from(base).saturating_add(delta).clamp(0, 100);
                u8::try_from(volume).ok().map(SpeakerRequest::Volume)
            }),
        };
        if let Some(request) = request {
            tokio::select! {
                biased;
                () = sent.closed() => {},
                _ = client.request_speaker(request) => {},
            }
        }
        let _ = sent.send(());
    }
}

pub(super) async fn command(name: &str, payload: Value, received: Instant) -> Value {
    if !payload.is_object() {
        return commands::invalid();
    }
    let change = match name {
        "set-volume" => serde_json::from_value::<SetVolume>(payload)
            .ok()
            .filter(|input| input.volume <= 100)
            .map(|input| Change::Absolute(SpeakerRequest::Volume(input.volume))),
        "adjust-volume" => serde_json::from_value::<AdjustVolume>(payload)
            .ok()
            .map(|input| Change::Relative(input.delta)),
        "set-mute" => serde_json::from_value::<SetOutputMute>(payload)
            .ok()
            .map(|input| Change::Absolute(SpeakerRequest::OutputMute(input.mute))),
        _ => None,
    };
    let Some(change) = change else {
        return commands::invalid();
    };
    if let Some(voice) = Voice::current() {
        voice.speaker.send(change, received + Duration::from_secs(1)).await;
    }
    commands::refusal("NOT_SUPPORTED", "The device did not acknowledge speaker control.")
}
