use super::voice::{View, Voice};
use echo_contracts::endpoint::{AdjustVolume, SetOutputMute, SetVolume, SpeakerReport};
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

/// How long a command may wait to reach the device link.
const SEND_WINDOW: Duration = Duration::from_secs(1);
/// How long, from receipt, a sent command waits for the device to report the
/// resulting speaker state. Port OS waits 60 s for the ack.
pub(super) const SETTLE_WINDOW: Duration = Duration::from_secs(3);

/// What became of one speaker command. Every command is acknowledged by one
/// report carrying its `requestId`.
pub(super) enum Outcome {
    /// The device reported this speaker state after the command was sent.
    Applied(SpeakerReport),
    Invalid,
    /// Sent, but the device never reported both volume and mute, so nothing
    /// confirms it.
    NoState,
    /// Sent, and the device reports speaker state, but no report reflected the
    /// change within the settle window.
    Unconfirmed,
    Dropped(&'static str),
}

impl Outcome {
    /// The ack's refusal code, or `None` when the ack carries `speaker`.
    /// `NOT_SUPPORTED` means this device cannot do it; `NOT_READY` is transient.
    pub(super) fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::Applied(_) => None,
            Self::Invalid => Some("INVALID_REQUEST"),
            // A relative change needs a reported base volume, which a device
            // that reports no speaker state never supplies.
            Self::NoState | Self::Dropped(VOLUME_UNKNOWN) => Some("NOT_SUPPORTED"),
            Self::Unconfirmed | Self::Dropped(_) => Some("NOT_READY"),
        }
    }
}

struct Request {
    change: Change,
    sent: oneshot::Sender<Result<SpeakerRequest, &'static str>>,
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

    /// Send one change; returns the request the device link accepted.
    async fn send(&self, change: Change, deadline: Instant) -> Result<SpeakerRequest, &'static str> {
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

    fn attached(&self) -> bool {
        self.0.borrow().is_some()
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
                result = client.request_speaker(request) => result.map(|()| request).map_err(|_| NO_SPEAKER_LINK),
            },
            Err(reason) => Err(reason),
        };
        let _ = sent.send(result);
    }
}

/// Apply one speaker command, then wait within the settle window for the
/// speaker state the device reports back.
pub(super) async fn command(name: &str, body: Value, received: Instant) -> Outcome {
    let Some(change) = change(name, body) else {
        return Outcome::Invalid;
    };
    let Some(voice) = Voice::current() else {
        return Outcome::Dropped(NO_SPEAKER_LINK);
    };
    let mut states = voice.subscribe();
    let before = states.borrow_and_update().speaker;
    let request = match voice.speaker.send(change, received + SEND_WINDOW).await {
        Ok(request) => request,
        Err(reason) => return Outcome::Dropped(reason),
    };
    // A state that already matches, or any newly reported state, settles the
    // command: the device may clamp or ignore a value.
    let settled =
        |view: &View| complete(view.speaker).is_some() && (view.speaker != before || applies(request, view.speaker));
    if let Ok(Ok(view)) = timeout_at(received + SETTLE_WINDOW, states.wait_for(settled)).await
        && let Some(speaker) = complete(view.speaker)
    {
        return Outcome::Applied(speaker);
    }
    if !voice.speaker.attached() {
        Outcome::Dropped(NO_SPEAKER_LINK)
    } else if complete(states.borrow().speaker).is_some() {
        Outcome::Unconfirmed
    } else {
        Outcome::NoState
    }
}

/// The reportable speaker state: both values known.
fn complete(speaker: SpeakerState) -> Option<SpeakerReport> {
    speaker
        .volume
        .zip(speaker.muted)
        .map(|(volume, muted)| SpeakerReport { volume, muted })
}

fn applies(request: SpeakerRequest, speaker: SpeakerState) -> bool {
    match request {
        SpeakerRequest::Volume(volume) => speaker.volume == Some(volume),
        SpeakerRequest::OutputMute(muted) => speaker.muted == Some(muted),
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
