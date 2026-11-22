use super::voice::{View, Voice};
use echo_contracts::endpoint::{AdjustVolume, SetOutputMute, SetVolume};
use echo_link::{EchoLink, SpeakerRequest, SpeakerState};
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio::{
    sync::{
        mpsc::{self, error::TrySendError},
        oneshot, watch,
    },
    time::{Instant, timeout_at},
};

#[derive(Clone, Default)]
pub(super) struct Control(watch::Sender<Option<mpsc::Sender<Request>>>);

enum Change {
    Absolute(SpeakerRequest),
    Relative(i64),
}

/// Fixed reasons a control reached no device.
const NO_SPEAKER_LINK: &str = "no_speaker_link";
const VOLUME_UNKNOWN: &str = "volume_unknown";
const BUSY: &str = "busy";

/// What became of one speaker command. Port OS keeps no pending request for
/// these, so a drop is traced on the bridge rather than answered.
pub(super) enum Outcome {
    /// Sent to the device link, which gives no applied acknowledgement.
    Sent,
    Invalid,
    Dropped(&'static str),
}

struct Request {
    change: Change,
    sent: oneshot::Sender<Result<(), &'static str>>,
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

    async fn send(&self, change: Change, deadline: Instant) -> Result<(), &'static str> {
        let (sent, wait) = oneshot::channel();
        let sender = self.0.borrow().clone().ok_or(NO_SPEAKER_LINK)?;
        if Instant::now() >= deadline {
            return Err(BUSY);
        }
        sender.try_send(Request { change, sent }).map_err(|error| match error {
            TrySendError::Full(_) => BUSY,
            TrySendError::Closed(_) => NO_SPEAKER_LINK,
        })?;
        match timeout_at(deadline, wait).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(NO_SPEAKER_LINK),
            Err(_) => Err(BUSY),
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
        // A relative change needs the device's reported volume as its base.
        let request = match change {
            Change::Absolute(request) => Ok(request),
            Change::Relative(delta) => view
                .borrow()
                .speaker
                .volume
                .and_then(|base| {
                    let volume = i64::from(base).saturating_add(delta).clamp(0, 100);
                    u8::try_from(volume).ok().map(SpeakerRequest::Volume)
                })
                .ok_or(VOLUME_UNKNOWN),
        };
        let result = match request {
            Ok(request) => tokio::select! {
                biased;
                () = sent.closed() => continue,
                result = client.request_speaker(request) => result.map_err(|_| NO_SPEAKER_LINK),
            },
            Err(reason) => Err(reason),
        };
        let _ = sent.send(result);
    }
}

/// Deliver one fire-and-forget speaker command.
pub(super) async fn command(name: &str, body: Value, received: Instant) -> Outcome {
    let Some(change) = change(name, body) else {
        return Outcome::Invalid;
    };
    let Some(voice) = Voice::current() else {
        return Outcome::Dropped(NO_SPEAKER_LINK);
    };
    match voice.speaker.send(change, received + Duration::from_secs(1)).await {
        Ok(()) => Outcome::Sent,
        Err(reason) => Outcome::Dropped(reason),
    }
}

fn change(name: &str, body: Value) -> Option<Change> {
    if !body.is_object() {
        return None;
    }
    match name {
        "set-volume" => serde_json::from_value::<SetVolume>(body)
            .ok()
            .filter(|input| (0.0..=100.0).contains(&input.volume))
            .and_then(|input| u8::try_from(round(input.volume)).ok())
            .map(|volume| Change::Absolute(SpeakerRequest::Volume(volume))),
        "adjust-volume" => serde_json::from_value::<AdjustVolume>(body)
            .ok()
            .filter(|input| input.volume_delta.is_finite())
            .map(|input| Change::Relative(round(input.volume_delta.clamp(-100.0, 100.0)))),
        "set-mute" => serde_json::from_value::<SetOutputMute>(body)
            .ok()
            .map(|input| Change::Absolute(SpeakerRequest::OutputMute(input.mute))),
        _ => None,
    }
}

/// Round a bounded volume value to the nearest whole step.
#[expect(clippy::cast_possible_truncation, reason = "callers bound the value to -100..=100")]
fn round(value: f64) -> i64 {
    value.round() as i64
}
