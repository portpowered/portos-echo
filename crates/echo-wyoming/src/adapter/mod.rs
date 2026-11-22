//! Converts the neutral link to the existing Wyoming client without a second queue.
use crate::{ClientOptions, EventStream, Run, WyomingClient};
use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
use echo_link::{
    CutStats, EchoLink, Error, LinkConnector, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PcmFormat, PlayOutcome,
    ReplySink, RunFailure, RunStats, SpeakerRequest, SpeakerState,
};
use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};
use tokio::sync::mpsc;

/// Configured transport selection; retry ownership remains in `WyomingClient`.
#[must_use]
pub fn connector(addr: SocketAddr, options: ClientOptions) -> impl LinkConnector {
    Connector { addr, options }
}
struct Connector {
    addr: SocketAddr,
    options: ClientOptions,
}
impl LinkConnector for Connector {
    type Link = WyomingClient;
    async fn connect(&self) -> Result<Self::Link, Error> {
        let mut client = WyomingClient::connect(self.addr, self.options.clone())
            .await
            .map_err(error)?;
        client.neutral_barrier = true;
        Ok(client)
    }
}
fn error(value: crate::Error) -> Error {
    match value {
        crate::Error::InvalidOptions => Error::InvalidOptions,
        crate::Error::Disconnected => Error::Transport,
        crate::Error::Protocol => Error::Protocol,
        crate::Error::Capacity => Error::Capacity,
        crate::Error::Timeout => Error::Timeout,
        crate::Error::Busy => Error::Busy,
        crate::Error::Closed => Error::Closed,
    }
}
fn wire(value: PcmFormat) -> AudioFormat {
    AudioFormat {
        rate: value.rate,
        width: value.width,
        channels: value.channels,
        extra: std::collections::BTreeMap::default(),
    }
}
impl MicrophoneRun for Run {
    fn wake_phrase(&self) -> &str {
        &self.pipeline().wake_word_name
    }
    async fn recv(&mut self) -> Result<Option<MicFrame>, Error> {
        Run::recv(self)
            .await
            .map(|frame| {
                frame.map(|frame| MicFrame {
                    format: PcmFormat {
                        rate: frame.data.rate,
                        width: frame.data.width,
                        channels: frame.data.channels,
                    },
                    payload: frame.payload,
                })
            })
            .map_err(error)
    }
    fn stats(&self) -> RunStats {
        let stats = Run::stats(self);
        RunStats {
            received: stats.received,
            dropped: stats.dropped,
        }
    }
}
mod mute_events;
mod speaker_events;
pub use mute_events::Events;
pub(crate) use speaker_events::Speaker;
/// The existing bounded producer with neutral send errors.
pub struct Sink(mpsc::Sender<Vec<u8>>);
impl ReplySink for Sink {
    async fn send(&self, frame: Vec<u8>) -> Result<(), Error> {
        self.0.send(frame).await.map_err(|_| Error::Closed)
    }
}
impl EchoLink for WyomingClient {
    type Events = Events;
    type ReplySink = Sink;
    type ReplySource = mpsc::Receiver<Vec<u8>>;
    fn events(&mut self) -> Result<Events, Error> {
        let barrier = self.neutral_barrier;
        Ok(Events::new(
            Self::events(self).map_err(error)?,
            barrier,
            self.speaker.clone(),
        ))
    }
    fn reply_channel(&self, capacity: NonZeroUsize) -> (Self::ReplySink, Self::ReplySource) {
        let (tx, rx) = mpsc::channel(capacity.get());
        (Sink(tx), rx)
    }
    async fn enable_capture(&self, failsafe_seconds: u32) -> Result<(), Error> {
        self.run_satellite_with_failsafe(failsafe_seconds).await.map_err(error)
    }
    async fn request_remote_capture(
        &self,
        request_id: std::sync::Weak<str>,
        deadline: std::time::Instant,
    ) -> Result<(), Error> {
        Self::request_remote_capture(self, request_id, deadline)
            .await
            .map_err(error)
    }
    async fn probe(&self) -> Result<(), Error> {
        self.describe().await.map(|_| ()).map_err(error)
    }
    async fn speaker_changed(&self, previous: SpeakerState) -> SpeakerState {
        self.speaker.changed(previous).await
    }
    async fn request_speaker(&self, request: SpeakerRequest) -> Result<(), Error> {
        let (name, data) = match request {
            SpeakerRequest::Volume(volume) if volume <= 100 => {
                ("hassmic-set-volume", serde_json::json!({"volume":volume}))
            }
            SpeakerRequest::OutputMute(mute) => ("hassmic-set-output-mute", serde_json::json!({"mute":mute})),
            SpeakerRequest::Volume(_) => return Err(Error::InvalidOptions),
        };
        self.send_speaker(name, data).await.map_err(error)
    }
    async fn play_with_timeout(
        &self,
        format: PcmFormat,
        source: Self::ReplySource,
        deadline: Duration,
    ) -> Result<PlayOutcome, Error> {
        Self::play_with_timeout(self, wire(format), source, deadline)
            .await
            .map(|outcome| match outcome {
                crate::PlayOutcome::Played => PlayOutcome::Played,
                crate::PlayOutcome::Cut => PlayOutcome::Cut,
            })
            .map_err(error)
    }
    async fn cut_with_stats(&self) -> Result<CutStats, Error> {
        Self::cut_with_stats(self)
            .await
            .map(|stats| CutStats {
                dropped_frames: stats.dropped_frames,
            })
            .map_err(error)
    }
    async fn pause(&self) -> Result<(), Error> {
        Self::pause(self).await.map_err(error)
    }
    async fn end_run(&self) -> Result<(), Error> {
        Self::end_run(self).await.map_err(error)
    }
    async fn thinking(&self, on: bool) -> Result<(), Error> {
        Self::thinking(self, on).await.map_err(error)
    }
    async fn fail_run(&self, code: RunFailure) -> Result<(), Error> {
        let (code, text) = match code {
            RunFailure::RelinkRequired => ("echo-relink-required", "Relink required."),
            RunFailure::KillSwitch => ("echo-kill-switch", "Kill switch enabled."),
            RunFailure::Unavailable => ("echo-unavailable", "Live unavailable."),
        };
        self.send_error(code, text).await.map_err(error)
    }
    async fn close(&self) -> Result<(), Error> {
        Self::close(self).await.map_err(error)
    }
}

#[cfg(test)]
mod echo_link_tests;

#[cfg(test)]
mod echo_link_recovery;

#[cfg(test)]
mod mute_tests;

#[cfg(test)]
mod remote_capture_tests;
