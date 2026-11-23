//! Prepared trace descriptors and the writer's drain port.
use serde_json::{Map, Value};
use std::sync::mpsc::SyncSender;

pub struct TraceRow {
    pub event: String,
    pub elapsed_ms: u64,
    pub mode: &'static str,
    pub run: Option<String>,
    pub fields: Map<String, Value>,
    pub payload_fields: &'static [&'static str],
    pub limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceFailure;

pub struct PendingTrace {
    pub row: TraceRow,
    pub receipt: Option<SyncSender<Result<(), TraceFailure>>>,
}

pub trait TraceDrain: Send + Sync {
    fn next_row(&self) -> Option<(u64, PendingTrace)>;

    /// Publish the outcome before releasing any checked receipt.
    fn completed(&self, sequence: u64, result: Result<(), TraceFailure>);
}
