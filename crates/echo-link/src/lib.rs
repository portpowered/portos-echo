//! Protocol-neutral device capture and acknowledged, bounded reply playback.
use std::{future::Future, num::NonZeroUsize, sync::Weak, time::Duration};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// PCM descriptor. Width is bytes per sample; no transport extensions.
pub struct PcmFormat {
    pub rate: u32,
    pub width: u16,
    pub channels: u16,
}
#[derive(Debug, PartialEq, Eq)]
pub struct MicFrame {
    pub format: PcmFormat,
    pub payload: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    pub received: u64,
    pub dropped: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutStats {
    pub dropped_frames: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayOutcome {
    Played,
    Cut,
}
/// Closed, secret-free reason for ending an unserved device run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunFailure {
    RelinkRequired,
    KillSwitch,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidOptions,
    Transport,
    Protocol,
    Capacity,
    Timeout,
    Busy,
    Closed,
}
pub enum LinkEvent<R> {
    Wake(R),
    RemoteCapture {
        request_id: String,
        run: R,
    },
    StaleRemoteCapture,
    Played,
    /// Effective device mute, ordered with wake and playback events.
    Mute(bool),
    /// Ordered count of suppressed microphone chunks, with no audio payload.
    MicrophoneDropped(u64),
    Other,
}
/// Queued chunks drain before EOF or a terminal error.
pub trait MicrophoneRun: Send {
    /// Phrase supplied by the device for this wake.
    fn wake_phrase(&self) -> &str;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    /// Device mute ends capture at its wire boundary. Pre-mute frames drain,
    /// then recv returns Ok(None) before the matching Mute(true) is delivered.
    /// Post-mute frames are never yielded and count in `RunStats::dropped`.
    fn recv(&mut self) -> impl Future<Output = Result<Option<MicFrame>, Error>> + Send;
    /// Received and evicted microphone chunks for this run only.
    fn stats(&self) -> RunStats;
}
pub trait LinkEvents: Send {
    type Run: MicrophoneRun;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn recv(&mut self) -> impl Future<Output = Result<Option<LinkEvent<Self::Run>>, Error>> + Send;
}
pub trait ReplySink: Send + Sync {
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn send(&self, frame: Vec<u8>) -> impl Future<Output = Result<(), Error>> + Send;
}
/// A single device connection owner, including its transport-specific recovery.
/// Playback completes only after acknowledgment, including empty and cut replies.
/// Cutting discards retained complete frames once and never replays them.
pub trait EchoLink: Send + Sync {
    type Events: LinkEvents;
    type ReplySink: ReplySink;
    type ReplySource: Send;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn events(&mut self) -> Result<Self::Events, Error>;
    /// One bounded source; its sender supplies backpressure to the producer.
    fn reply_channel(&self, capacity: NonZeroUsize) -> (Self::ReplySink, Self::ReplySource);
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    /// The stalled-progress watchdog must be in 1..=2147483647 seconds.
    /// Invalid values fail before any device effect.
    fn enable_capture(&self, failsafe_seconds: u32) -> impl Future<Output = Result<(), Error>> + Send;
    /// Request capture while the caller owns the corresponding strong request ID.
    /// A matching device response arrives as `LinkEvent::RemoteCapture`.
    /// # Errors
    /// Returns a fixed category if the link cannot send the request.
    fn request_remote_capture(
        &self,
        request_id: Weak<str>,
        deadline: std::time::Instant,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        let _ = (request_id, deadline);
        std::future::ready(Err(Error::InvalidOptions))
    }
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn probe(&self) -> impl Future<Output = Result<(), Error>> + Send;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn play_with_timeout(
        &self,
        format: PcmFormat,
        source: Self::ReplySource,
        deadline: Duration,
    ) -> impl Future<Output = Result<PlayOutcome, Error>> + Send;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn cut_with_stats(&self) -> impl Future<Output = Result<CutStats, Error>> + Send;
    /// End a successful current run, keeping the connection and wake registration.
    /// Repeated calls without a current run have no effect. Playback must settle first.
    /// # Errors
    /// Busy while playback is unsettled, or a fixed transport failure category.
    fn end_run(&self) -> impl Future<Output = Result<(), Error>> + Send;
    /// End the current run on the device; the connection stays up.
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn fail_run(&self, code: RunFailure) -> impl Future<Output = Result<(), Error>> + Send;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn pause(&self) -> impl Future<Output = Result<(), Error>> + Send;
    /// Show on the device that delegated work is running in silence. Display
    /// only: capture, playback and the run are unaffected. The caller clears
    /// it when reply audio plays; the device clears it when the run ends.
    /// Links without a face accept and ignore it.
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn thinking(&self, on: bool) -> impl Future<Output = Result<(), Error>> + Send {
        let _ = on;
        std::future::ready(Ok(()))
    }
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn close(&self) -> impl Future<Output = Result<(), Error>> + Send;
}
/// Composition boundary; the adapter owns retries and reconnect registration.
/// Each connection starts unmuted. A muted device supplies Mute(true) first;
/// absence of a mute snapshot means unmuted. Handshake barriers stay in adapters.
pub trait LinkConnector: Send + Sync {
    type Link: EchoLink;
    /// # Errors
    /// Returns a fixed category on failure; no peer data enters diagnostics.
    fn connect(&self) -> impl Future<Output = Result<Self::Link, Error>> + Send;
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidOptions => "invalid link options",
            Self::Transport => "connection lost",
            Self::Protocol => "invalid device frame",
            Self::Capacity => "bounded capacity exhausted",
            Self::Timeout => "operation deadline elapsed",
            Self::Busy => "operation already active",
            Self::Closed => "link closed",
        })
    }
}
impl std::error::Error for Error {}

#[cfg(feature = "fake")]
pub mod fake;
