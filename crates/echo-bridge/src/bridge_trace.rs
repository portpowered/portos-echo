//! Closed event vocabulary and counters only. Never serialize provider events.
//! `reply.cut` also reports `bridge_evicted` (prior capacity evictions) and
//! `reply_frames` (complete decoded frames offered to the bridge queue). Neither
//! prior evictions nor partial tails belong to the retained `dropped_frames`.
//! `reply.overflow` records the first bridge reply eviction per run, with its
//! fixed frame capacity. Final eviction totals remain in `reply.cut`.
use crate::Error;
use serde_json::{Map, Value};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};

pub struct Trace {
    writer: File,
    started: Instant,
    mode: &'static str,
}

impl Trace {
    /// # Errors
    /// Returns a safe I/O category; paths never enter diagnostics.
    pub fn open(path: &Path, mode: &'static str) -> Result<Self, Error> {
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| Error::Trace)?;
        Ok(Self {
            writer,
            started: Instant::now(),
            mode,
        })
    }

    /// Callers supply only authored event names and safe, bounded fields.
    /// # Errors
    /// A serialization/write/flush failure prevents claiming completion.
    pub fn record(&mut self, event: &str, run: Option<&str>, fields: Value) -> Result<(), Error> {
        let mut entry = Map::new();
        entry.insert("event".into(), event.into());
        entry.insert(
            "elapsed_ms".into(),
            u64::try_from(self.started.elapsed().as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        );
        entry.insert("provider_auth_mode".into(), self.mode.into());
        if let Some(run) = run {
            entry.insert("run_id".into(), run.into());
        }
        if let Value::Object(fields) = fields {
            entry.extend(fields);
        }
        let bytes = serde_json::to_vec(&entry).map_err(|_| Error::Trace)?;
        if bytes.len() > 4096 {
            return Err(Error::Trace);
        }
        self.writer
            .write_all(&bytes)
            .and_then(|()| self.writer.write_all(b"\n"))
            .and_then(|()| self.writer.flush())
            .map_err(|_| Error::Trace)
    }
}

#[cfg(test)]
mod bridge_spine_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bridge_spine_trace_write_failure_is_sanitized_and_keeps_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        std::fs::write(&path, b"prior record\n").unwrap();
        let mut trace = Trace {
            writer: File::open(&path).unwrap(), // Read-only sink injects an actual write error.
            started: Instant::now(),
            mode: "chatgpt",
        };
        let error = trace.record("run.started", Some("run-1"), json!({})).unwrap_err();
        assert_eq!(error, Error::Trace);
        assert_eq!(error.to_string(), "trace");
        assert_eq!(std::fs::read(path).unwrap(), b"prior record\n");
    }

    #[test]
    fn bridge_spine_oversized_trace_is_rejected_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        assert_eq!(
            trace.record("run.started", Some("run-1"), json!({"reason":"x".repeat(4096)})),
            Err(Error::Trace)
        );
        assert_eq!(std::fs::read(&path).unwrap(), Vec::<u8>::new());
        trace.record("run.started", Some("run-1"), json!({})).unwrap();
        let line: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(line["provider_auth_mode"], "api_key");
        assert_eq!(line["event"], "run.started");
        assert_eq!(line["run_id"], "run-1");
    }
}
