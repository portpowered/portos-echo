//! Typed Wyoming microphone runs and acknowledged reply playback.
//!
//! All queues are bounded. Microphone overflow evicts the oldest frame;
//! lifecycle overflow closes the connection. Audio is never logged or persisted.
//! This initial connection spine does not reconnect automatically.
#[cfg(test)]
extern crate self as echo_wyoming;

mod wyoming_client;
mod wyoming_options;
mod wyoming_queue;
mod wyoming_route;
mod wyoming_session;
mod wyoming_wire;

#[cfg(test)]
mod wyoming_test_support;
#[cfg(test)]
mod wyoming_tests;

pub use wyoming_client::WyomingClient;
pub use wyoming_options::ClientOptions;

use echo_contracts::wyoming::{AudioChunkData, RunPipelineData, WyomingEvent};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use wyoming_queue::MicQueue;

/// Fixed error categories never include microphone bytes or peer data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid client options")]
    InvalidOptions,
    #[error("connection lost")]
    Disconnected,
    #[error("invalid Wyoming frame")]
    Protocol,
    #[error("bounded capacity exhausted")]
    Capacity,
    #[error("operation deadline elapsed")]
    Timeout,
    #[error("operation already active")]
    Busy,
    #[error("client closed")]
    Closed,
}

/// One microphone chunk with the format reported by that chunk.
#[derive(Debug, PartialEq)]
pub struct MicFrame {
    pub data: AudioChunkData,
    pub payload: Vec<u8>,
}

/// Counts scoped to one run, including frames evicted before consumption.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    pub received: u64,
    pub dropped: u64,
}

/// A typed event, with a new microphone handle only on `run-pipeline`.
pub struct ReceivedEvent {
    pub event: WyomingEvent,
    pub payload: Vec<u8>,
    pub run: Option<Run>,
}

/// Playback acknowledgment after hassmic drains the transmitted audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayOutcome {
    Played,
    Cut,
}

/// The sole receiver for this client's bounded event queue.
pub struct EventStream {
    receiver: mpsc::Receiver<ReceivedEvent>,
    terminal: watch::Receiver<Option<Error>>,
}

impl EventStream {
    /// Receive events in wire order, then the terminal connection error.
    /// # Errors
    /// Returns the connection's fixed failure category after queued events drain.
    pub async fn recv(&mut self) -> Result<Option<ReceivedEvent>, Error> {
        match self.receiver.recv().await {
            Some(event) => Ok(Some(event)),
            None => match *self.terminal.borrow() {
                Some(Error::Closed) | None => Ok(None),
                Some(error) => Err(error),
            },
        }
    }
}

/// A microphone run; replacing a pipeline closes the previous run.
pub struct Run {
    pipeline: RunPipelineData,
    queue: Arc<MicQueue>,
}

impl Run {
    #[must_use]
    pub fn pipeline(&self) -> &RunPipelineData {
        &self.pipeline
    }

    /// Receive a microphone frame, or end after the run's queued frames drain.
    /// # Errors
    /// Returns the generation failure after its queued frames drain.
    pub async fn recv(&mut self) -> Result<Option<MicFrame>, Error> {
        self.queue.recv().await
    }

    #[must_use]
    pub fn stats(&self) -> RunStats {
        self.queue.stats()
    }
}
